use std::ops::RangeInclusive;

use crate::{
    bridge::{self, DataSenderBridge, IdDataSenderBridge},
    event,
};
use anyhow::Context as _;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use uuid::Uuid;

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
/// and wait to receive the first message which is [`tunneld_pkg::bridge::BridgeData::Sender`] from the control server.
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
        .unwrap();
    let data_sender = bridge_chan_receiver
        .recv()
        .await
        .context("failed to receive data_sender")
        .unwrap();
    let data_sender = {
        match data_sender {
            bridge::BridgeData::Sender(sender) => sender,
            _ => panic!("we expect to receive DataSender from data_channel_rx at the first time."),
        }
    };

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
    free_port_range: RangeInclusive<u16>,
) -> anyhow::Result<(u16, T::Output), Status> {
    if port > 0 {
        let socket = T::create_socket(port).await?;
        Ok((port, socket))
    } else {
        // refer: https://github.com/ekzhang/bore/blob/v0.5.1/src/server.rs#L88
        // todo: a better way to find a free port
        for _ in 0..150 {
            let freeport = fastrand::u16(free_port_range.clone());
            let result = T::create_socket(freeport).await;
            if result.is_err() {
                continue;
            }
            return Ok((freeport, result.unwrap()));
        }
        Err(Status::internal("failed to find a free port"))
    }
}
