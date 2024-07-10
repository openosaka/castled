use tokio::{net::UdpSocket, select, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::error;
use tunneld_pkg::event;

pub(crate) struct Udp {
    listener: UdpSocket,
    conn_event_sender: mpsc::Sender<event::UserIncoming>,
}

impl Udp {
    pub(crate) fn new(
        listener: UdpSocket,
        conn_event_sender: mpsc::Sender<event::UserIncoming>,
    ) -> Self {
        Self {
            listener,
            conn_event_sender,
        }
    }

    pub async fn serve(self, shutdown: CancellationToken) {
        loop {
            let mut buf = [0; 1024];

            select! {
                _ = shutdown.cancelled() => {
                    return;
                }
                result = self.listener.recv_from(&mut buf) => {
                    match result {
                        Ok(data) => {
                            let (n, addr) = data;
                            let buf = &buf[..n];
                            //
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
