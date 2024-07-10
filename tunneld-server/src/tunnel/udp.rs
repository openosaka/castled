use std::net::SocketAddr;

use tokio::{net::UdpSocket, select, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::error;
use tunneld_pkg::event;

use crate::tunnel::BridgeResult;

pub(crate) struct Udp {
    listener: UdpSocket,
    user_incoming_sender: mpsc::Sender<event::UserIncoming>,
}

impl Udp {
    pub(crate) fn new(
        listener: UdpSocket,
        user_incoming_sender: mpsc::Sender<event::UserIncoming>,
    ) -> Self {
        Self {
            listener,
            user_incoming_sender,
        }
    }

    pub async fn serve(self, shutdown: CancellationToken) {
        let user_incoming_sender = self.user_incoming_sender.clone();

        let BridgeResult {
            data_sender,
            data_receiver,
            client_cancel_receiver,
            remove_bridge_sender,
        } = super::init_data_sender_bridge(user_incoming_sender.clone())
            .await
            .unwrap();

        let (transfer_tx, mut transfer_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(1024);
        let shutdown_listener = shutdown.clone();

        tokio::spawn(async move {
            loop {
                select! {
                    _ = shutdown.cancelled() => {
                        return;
                    }
                    result = transfer_rx.recv() => {
                        match result {
                            Some((data, addr)) => {
                                // send the traffic data to the client
                            }
                            None => {
                                return;
                            }
                        }
                    }

                }
            }
        });

        loop {
            let mut buf = [0; 1024];

            select! {
                _ = shutdown_listener.cancelled() => {
                    return;
                }
                result = self.listener.recv_from(&mut buf) => { // read from user
                    match result {
                        Ok(data) => {
                            let (n, addr) = data;
                            let buf = &buf[..n];
                            transfer_tx.send((buf.to_vec(), addr)).await.unwrap();
                        },
                        Err(err) => {
                            error!(err = ?err, "failed to receive data");
                            return;
                        },
                    }
                }
            }
        }
    }
}
