use anyhow::Context as _;
use bytes::Bytes;
use tokio::{
    io::{self, AsyncWriteExt as _},
    select,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tunneld_pkg::{
    bridge, event,
    io::{StreamingReader, StreamingWriter, VecWrapper},
};
use uuid::Uuid;

pub(crate) async fn handle_tcp_listener(
    listener: tokio::net::TcpListener,
    shutdown: CancellationToken,
    conn_event_chan: mpsc::Sender<event::UserInbound>,
) {
    loop {
        select! {
            _ = shutdown.cancelled() => {
                return;
            }
            result = async {
                match listener.accept().await  {
                    Ok(result) => {
                        Some(result)
                    }
                    Err(err) => {
                        debug!("failed to accept connection: {:?}", err);
                        None
                    }
                }
            } => {
                if result.is_none() {
                    return;
                }

                let bridge_id = Bytes::from(Uuid::new_v4().to_string());
                let (bridge_chan, mut bridge_chan_tx) = mpsc::channel(1024);

                let client_cancel = CancellationToken::new();
                let client_cancel_receiver = client_cancel.clone();

                let event = bridge::IdDataSenderBridge{
                    id: bridge_id.clone(),
                    inner: bridge::DataSenderBridge{
                        chan: bridge_chan.clone(),
                        cancel: client_cancel,
                    }
                };
                conn_event_chan.send(event::UserInbound::Add(event)).await.unwrap();
                let data_sender = bridge_chan_tx.recv().await.context("failed to receive data_sender").unwrap();
                let data_sender = {
                    match data_sender {
                        bridge::BridgeData::Sender(sender) => sender,
                        _ => panic!("we expect to receive DataSender from data_channel_rx at the first time."),
                    }
                };

                let (stream, _addr) = result.unwrap();
                let conn_event_chan_for_removing = conn_event_chan.clone();
                tokio::spawn(async move {
                    let (mut remote_reader, mut remote_writer) = stream.into_split();
                    let wrapper = VecWrapper::<Vec<u8>>::new();
                    // we expect to receive data from data_channel_rx after receive the first data_sender
                    let mut tunnel_reader = StreamingReader::new(bridge_chan_tx);
                    let mut tunnel_writer = StreamingWriter::new(data_sender, wrapper);
                    let remote_to_me_to_tunnel = async {
                        io::copy(&mut remote_reader, &mut tunnel_writer).await.unwrap();
                        tunnel_writer.shutdown().await.context("failed to shutdown tunnel writer").unwrap();
                        debug!("finished the transfer between remote and tunnel");
                    };
                    let tunnel_to_me_to_remote = async {
                        io::copy(&mut tunnel_reader, &mut remote_writer).await.unwrap();
                        remote_writer.shutdown().await.context("failed to shutdown remote writer").unwrap();
                        debug!("finished the transfer between tunnel and remote");
                    };

                    tokio::select! {
                        _ = async { tokio::join!(remote_to_me_to_tunnel, tunnel_to_me_to_remote) } => {
                            conn_event_chan_for_removing
                                .send(event::UserInbound::Remove(bridge_id))
                                .await
                                .context("notify server to remove connection channel")
                                .unwrap();
                        }
                        _ = client_cancel_receiver.cancelled() => {
                            let _ = remote_writer.shutdown().await;
                            let _ = tunnel_writer.shutdown().await;
                        }
                    }
                });
            }
        }
    }
}
