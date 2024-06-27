use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tonic::Status;

pub struct Event {
	pub resp: oneshot::Sender<Option<Status>>,
	pub payload: Payload,
}

pub enum Payload {
	TcpRegister{
		port: u16,
		cancel: CancellationToken,
		new_connection_sender: mpsc::Sender<Connection>,
	},
}

pub struct Connection {
    pub id: String,
	// this channel has two purposes:
	// 1. when the server receives `Start` action from `data` streaming,
	// the server will send `DataSender` through this channel,
	// 2. when the server receives `Sending` from `data` streaming,
	// the server will send `Data` through this channel.
    pub channel: mpsc::Sender<ConnectionChannelDataType>,
}

pub enum ConnectionChannelDataType {
	DataSender(mpsc::Sender<Vec<u8>>),
	Data(Vec<u8>),
}
