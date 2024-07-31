use crate::{
    bridge::{self, DataSenderBridge, IdDataSenderBridge},
    event,
};
use anyhow::Context as _;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::error;
use uuid::Uuid;

use super::port::{Available, PortManager};

pub(crate) mod http;
pub(crate) mod tcp;
pub(crate) mod udp;

pub(crate) struct BridgeResult {
    pub data_sender: mpsc::Sender<Vec<u8>>,
    pub data_receiver: mpsc::Receiver<bridge::BridgeData>,
    pub client_cancel_receiver: CancellationToken,
    /// the caller should cancel this token when it finishes the transfer.
    pub remove_bridge_sender: CancellationToken,
}

/// init_data_sender_bridge creates a bridge between the control server and data server.
///
/// In this function, it has been sent the bridge to the control server,
/// and wait to receive the first message which is [`crate::bridge::BridgeData::Sender`] from the control server.
pub(crate) async fn init_data_sender_bridge(
    user_incoming_chan: mpsc::Sender<event::UserIncoming>,
) -> anyhow::Result<BridgeResult> {
    let bridge_id = Bytes::from(Uuid::new_v4().to_string());
    let (bridge_chan, mut bridge_chan_receiver) = mpsc::channel(1024);

    let client_cancel = CancellationToken::new();
    let client_cancel_receiver = client_cancel.clone();

    let event = IdDataSenderBridge {
        id: bridge_id.clone(),
        inner: DataSenderBridge::new(bridge_chan.clone(), client_cancel),
    };
    user_incoming_chan
        .send(event::UserIncoming::Add(event))
        .await
        .context("send user incoming event, this operation should not fail")
        .unwrap();

    let remove_bridge_sender = CancellationToken::new();
    let remove_bridge_receiver = remove_bridge_sender.clone();
    let bridge_id_clone = bridge_id.clone();
    tokio::spawn(async move {
        remove_bridge_receiver.cancelled().await;
        user_incoming_chan
            .send(event::UserIncoming::Remove(bridge_id_clone))
            .await
            .context("notify server to remove connection channel")
            .unwrap();
    });

    let data_sender = tokio::select! {
        data_sender = bridge_chan_receiver.recv() => {
            match data_sender {
                Some(bridge::BridgeData::Sender(sender)) => sender,
                _ => panic!("we expect to receive DataSender from data_channel_rx at the first time."),
            }
        }
        _ = client_cancel_receiver.cancelled() => {
            remove_bridge_sender.cancel();
            return Err(anyhow::anyhow!("client cancelled"))
        }
    };

    Ok(BridgeResult {
        data_sender,
        data_receiver: bridge_chan_receiver,
        client_cancel_receiver,
        remove_bridge_sender,
    })
}

pub(crate) trait SocketCreator {
    type Output;

    async fn create_socket(port: u16) -> anyhow::Result<Self::Output, Status>;
}

pub(crate) async fn create_socket<T: SocketCreator>(
    port: u16,
    port_manager: &mut PortManager,
) -> anyhow::Result<(Available, T::Output), Status> {
    if port > 0 {
        if !port_manager.allow(port) {
            return Err(Status::invalid_argument(
                "port is not in the allowed range or is excluded",
            ));
        }

        match port_manager.take(port) {
            None => Err(Status::already_exists("port is already in use")),
            Some(mut available_port) => match T::create_socket(port).await {
                Err(e) => {
                    available_port.unavailable();
                    Err(e)
                }
                Ok(socket) => Ok((available_port, socket)),
            },
        }
    } else {
        for _ in 0..150 {
            let mut available_port: Available = match port_manager.get() {
                None => {
                    return Err(Status::resource_exhausted("no available port"));
                }
                Some(port) => port,
            };
            let port = *available_port;
            match T::create_socket(port).await {
                Err(err) => {
                    error!(?err, port, "failed to create socket");
                    available_port.unavailable();
                    continue;
                }
                Ok(socket) => {
                    return Ok((available_port, socket));
                }
            }
        }
        Err(Status::resource_exhausted("no available port"))
    }
}
