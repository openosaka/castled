use std::{net::SocketAddr, sync::Arc};

use crate::{bridge::BridgeData, event, helper::create_udp_socket, server::tunnel::BridgeResult};
use dashmap::DashMap;
use tokio::{net::UdpSocket, select, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::{debug, error};

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
        let socket2 = Arc::clone(&socket);

        let shutdown_listener = shutdown.clone();
        let (data_sender, data_receiver) = mpsc::channel(128);
        let transfer_manager =
            TransferManager::new(self.user_incoming_sender.clone(), data_receiver);
        tokio::spawn(async move {
            transfer_manager.run(socket2).await;
        });

        loop {
            let mut buf = [0; MAX_DATAGRAM_SIZE];

            select! {
                _ = shutdown_listener.cancelled() => {
                    return;
                }
                result = socket.recv_from(&mut buf) => { // read from user
                    match result {
                        Ok(data) => {
                            // since udp is connectionless,
                            // when receive a packet from addr, we treat it as a keepalive.
                            // so there should be a auto release mechanism.
                            // TODO(sword): add a keepalive mechanism for udp.

                            let (n, addr) = data;
                            data_sender.send((buf[..n].to_vec(), addr)).await.unwrap();
                        },
                        Err(err) => {
                            error!(err = ?err, "failed to receive data");
                        },
                    }
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

struct TransferManager {
    user_incoming_sender: mpsc::Sender<event::UserIncoming>,
    data_receiver: mpsc::Receiver<(Vec<u8>, SocketAddr)>,
}

impl TransferManager {
    fn new(
        user_incoming_sender: mpsc::Sender<event::UserIncoming>,
        data_receiver: mpsc::Receiver<(Vec<u8>, SocketAddr)>,
    ) -> Self {
        Self {
            user_incoming_sender,
            data_receiver,
        }
    }

    async fn run(mut self, socket: Arc<UdpSocket>) {
        let transferring = Arc::new(DashMap::new());

        loop {
            let transferring = Arc::clone(&transferring);
            select! {
                data = self.data_receiver.recv() => {
                    let socket = Arc::clone(&socket);
                    let (data, socket_addr) = match data {
                        Some(data) => data,
                        None => {
                            return;
                        }
                    };

                    if !transferring.contains_key(&socket_addr) {
                        let BridgeResult {
                            data_sender,
                            data_receiver,
                            client_cancel_receiver,
                            remove_bridge_sender,
                        } = match super::init_data_sender_bridge(self.user_incoming_sender.clone()).await {
                            Ok(result) => result,
                            Err(err) => {
                                error!(err = ?err, "failed to init data sender bridge");
                                return
                            }
                        };

                        let (transfer_tx, transfer_rx) = mpsc::channel(128);
                        transferring.insert(socket_addr, transfer_tx.clone());
                        debug!(?socket_addr, "new transfer");

                        tokio::spawn(async move {
                            TransferManager::transfer(transfer_rx, client_cancel_receiver, data_sender, data_receiver, &socket, socket_addr).await;
                            transferring.remove(&socket_addr);
                            remove_bridge_sender.cancel();
                        });
                        transfer_tx.send(data).await.unwrap();
                    } else {
                        debug!(?socket_addr, "send data to transfer");
                        transferring.get(&socket_addr).unwrap().send(data).await.unwrap();
                    }
                }
            }
        }
    }

    async fn transfer(
        mut transfer_rx: mpsc::Receiver<Vec<u8>>,
        client_cancel_receiver: CancellationToken,
        data_sender: mpsc::Sender<Vec<u8>>,
        mut data_receiver: mpsc::Receiver<BridgeData>,
        remote_writer: &UdpSocket,
        remote_addr: SocketAddr,
    ) {
        let read_transfer_send_to_bridge = async {
            loop {
                select! {
                    _ = client_cancel_receiver.cancelled() => {}
                    data = transfer_rx.recv() => {
                        match data {
                            None => return,
                            Some(data) => {
                                select! {
                                    _ = client_cancel_receiver.cancelled() => {}
                                    result = data_sender.send(data) => {
                                        if let Err(err) = result {
                                            error!(err = ?err, "failed to send udp to client");
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };

        let read_bridge_send_to_user = async {
            loop {
                select! {
                    _ = client_cancel_receiver.cancelled() => {
                        return;
                    }
                    result = data_receiver.recv() => {
                        if result.is_none() {
                            return;
                        }
                        match result.unwrap() {
                            BridgeData::Data(data) => {
                                select! {
                                    _ = client_cancel_receiver.cancelled() => {
                                        return;
                                    }
                                    result = remote_writer.send_to(&data, remote_addr) => {
                                        if let Err(err) = result {
                                            error!(err = ?err, "failed to send udp to client");
                                            return;
                                        }
                                    }
                                }
                            },
                            BridgeData::Sender(_) => {
                                panic!("only expect data");
                            },
                        };
                    }
                }
            }
        };

        tokio::join!(read_transfer_send_to_bridge, read_bridge_send_to_user);
    }
}
