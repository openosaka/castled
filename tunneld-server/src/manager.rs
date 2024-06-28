use std::sync::Arc;

use anyhow::Context as _;
use tokio::io::AsyncWriteExt; // for shutdown() method
use tokio::{io, select, spawn, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use tunneld_pkg::{
    event,
    io::{StreamingReader, StreamingWriter, VecWrapper},
    util::create_listener,
};
use uuid::Uuid;

pub struct TcpManager {}

impl TcpManager {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn handle_listeners(self, mut receiver: mpsc::Receiver<event::Event>) {
        let this = Arc::new(self);

        while let Some(event) = receiver.recv().await {
            match event.payload {
                event::Payload::TcpRegister {
                    port,
                    cancel,
                    new_connection_sender,
                } => match create_listener(port).await {
                    Ok(listener) => {
                        let this = Arc::clone(&this);
                        spawn(async move {
                            this.handle_listener(listener, cancel, new_connection_sender)
                                .await;
                            debug!("tcp listener on {} closed", port);
                        });
                        event.resp.send(None).unwrap(); // success
                    }
                    Err(status) => {
                        event.resp.send(Some(status)).unwrap();
                    }
                },
            }
        }

        debug!("tcp manager quit");
    }

    async fn handle_listener(
        &self,
        listener: tokio::net::TcpListener,
        shutdown: CancellationToken,
        new_connection_sender: mpsc::Sender<event::Connection>,
    ) {
        loop {
            select! {
                _ = shutdown.cancelled() => {
                    return;
                }
                result = listener.accept() => {
                    let (stream, addr) = result.unwrap();
                    let connection_id = Uuid::new_v4().to_string();
                    let (data_channel, mut data_channel_rx) = mpsc::channel(1024);
                    debug!("new user connection {} from: {:?}", connection_id, addr);

                    let cancel_w = CancellationToken::new();
                    let cancel = cancel_w.clone();

                    let event = event::Connection{
                        id: connection_id.clone(),
                        channel: data_channel.clone(),
                        cancel: cancel_w,
                    };
                    new_connection_sender.send(event).await.unwrap();
                    let data_sender = data_channel_rx.recv().await.context("failed to receive data_sender").unwrap();
                    let data_sender = {
                        match data_sender {
                            event::ConnectionChannelDataType::DataSender(sender) => sender,
                            _ => panic!("we expect to receive DataSender from data_channel_rx at the first time."),
                        }
                    };

                    tokio::spawn(async move {
                        let (mut remote_reader, mut remote_writer) = stream.into_split();
                        let wrapper = VecWrapper::<Vec<u8>>::new();
                        let mut tunnel_writer = StreamingWriter::new(data_sender, wrapper);
                        let mut tunnel_reader = StreamingReader::new(data_channel_rx); // we expect to receive data from data_channel_rx after the first time.
                        let remote_to_me_to_tunnel = async {
                            debug!("start to transfer data from remote to tunnel");
                            io::copy(&mut remote_reader, &mut tunnel_writer).await.unwrap();
                            tunnel_writer.shutdown().await.context("failed to shutdown tunnel writer").unwrap();
                            debug!("finished the transfer between remote and tunnel");
                        };
                        let tunnel_to_me_to_remote = async {
                            debug!("start to transfer data from tunnel to remote");
                            io::copy(&mut tunnel_reader, &mut remote_writer).await.unwrap();
                            remote_writer.shutdown().await.context("failed to shutdown remote writer").unwrap();
                            debug!("finished the transfer between tunnel and remote");
                        };

                        tokio::select! {
                            _ = async { tokio::join!(remote_to_me_to_tunnel, tunnel_to_me_to_remote) } => {
                                debug!("finished the transfer between remote and tunnel");
                            }
                            _ = cancel.cancelled() => {
                                let _ = remote_writer.shutdown().await;
                                let _ = tunnel_writer.shutdown().await;
                            }
                        }
                        debug!("closed connection {}", connection_id);
                    });
                }
            }
        }
    }
}
