use std::sync::Arc;

use tokio::{net::UdpSocket, select, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::{error, warn};
use tunneld_pkg::{bridge::BridgeData, event, util::create_udp_socket};

use crate::tunnel::BridgeResult;

use super::SocketCreator;

const MAX_DATAGRAM_SIZE: usize = 65507;

pub(crate) struct Udp {
    socket: UdpSocket,
    user_incoming_sender: mpsc::Sender<event::UserIncoming>,
}

impl Udp {
    pub(crate) fn new(
        socket: UdpSocket,
        user_incoming_sender: mpsc::Sender<event::UserIncoming>,
    ) -> Self {
        Self {
            socket,
            user_incoming_sender,
        }
    }

    pub async fn serve(self, shutdown: CancellationToken) {
        let socket = Arc::new(self.socket);

        let shutdown_listener = shutdown.clone();

        // read from user(request)
        loop {
            let mut buf = [0; MAX_DATAGRAM_SIZE]; // TODO(sword): use a buffer pool
            let remote_reader = Arc::clone(&socket);
            let remote_writer = Arc::clone(&socket);

            select! {
                _ = shutdown_listener.cancelled() => {
                    return;
                }
                result = remote_reader.recv_from(&mut buf) => { // read from user
                    let user_incoming_sender = self.user_incoming_sender.clone();
                    tokio::spawn(async move {
                        match result {
                            Ok(data) => {
                                let (n, addr) = data;
                                let BridgeResult {
                                    data_sender,
                                    mut data_receiver,
                                    client_cancel_receiver,
                                    remove_bridge_sender,
                                } = super::init_data_sender_bridge(user_incoming_sender.clone())
                                    .await
                                    .unwrap();

                                select!{
                                    _ = client_cancel_receiver.cancelled() => {}
                                    result = data_sender.send(buf[..n].to_vec()) => {
                                        if let Err(err) = result {
                                            remove_bridge_sender.cancel();
                                            error!(err = ?err, "failed to send udp to client");
                                            return;
                                        }
                                    }
                                }


                                let mut data = vec![];
                                select !{
                                    _ = client_cancel_receiver.cancelled() => {}
                                    result = data_receiver.recv() => {
                                        if result.is_none() {
                                            remove_bridge_sender.cancel();
                                            warn!("received a empty data from data_receiver, shouldn't happen");
                                            return;
                                        }
                                        data = match result.unwrap() {
                                            BridgeData::Data(data) => data,
                                            BridgeData::Sender(_) => {
                                                panic!("data_receiver should not be closed");
                                            },
                                        };
                                    }
                                }

                                select!{
                                    _ = client_cancel_receiver.cancelled() => {}
                                    result = remote_writer.send_to(data.as_slice(), addr) => {
                                        if let Err(err) = result {
                                            error!(err = ?err, "failed to send udp response");
                                        }
                                    }
                                }
                                remove_bridge_sender.cancel();
                            },
                            Err(err) => {
                                error!(err = ?err, "failed to receive data");
                            },
                        }
                    });
                }
            }
        }
    }
}

impl SocketCreator for Udp {
    type Output = UdpSocket;

    async fn create_socket(port: u16) -> anyhow::Result<UdpSocket, Status> {
        create_udp_socket(port).await
    }
}
