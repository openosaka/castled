use crate::tunnel::BridgeResult;
use anyhow::Context as _;
use tokio::{
    io::{self, AsyncWriteExt as _},
    net::TcpListener,
    select,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};
use tunneld_pkg::{
    event,
    io::{StreamingReader, StreamingWriter, VecWrapper},
};

pub struct Tcp {
    listener: TcpListener,
    user_incoming_sender: mpsc::Sender<event::UserIncoming>,
}

impl Tcp {
    pub fn new(
        listener: TcpListener,
        user_incoming_sender: mpsc::Sender<event::UserIncoming>,
    ) -> Self {
        Self {
            listener,
            user_incoming_sender,
        }
    }

    pub async fn serve(self, shutdown: CancellationToken) {
        loop {
            select! {
                _ = shutdown.cancelled() => {
                    return;
                }
                result = async {
                    match self.listener.accept().await  {
                        Ok(result) => {
                            Some(result)
                        }
                        Err(err) => {
                            error!(err = ?err, "failed to accept connection");
                            None
                        }
                    }
                } => {
                    if result.is_none() {
                        return;
                    }

                    let user_incoming_sender = self.user_incoming_sender.clone();

                    let BridgeResult{
                        data_sender,
                        data_receiver,
                        client_cancel_receiver,
                        remove_bridge_sender
                    } = super::init_data_sender_bridge(user_incoming_sender.clone()).await.unwrap();

                    let (stream, _addr) = result.unwrap();
                    tokio::spawn(async move {
                        let (mut remote_reader, mut remote_writer) = stream.into_split();
                        let wrapper = VecWrapper::<Vec<u8>>::new();
                        // we expect to receive data from data_channel_rx after receive the first data_sender
                        let mut tunnel_reader = StreamingReader::new(data_receiver);
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
                                remove_bridge_sender.cancel();
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
}
