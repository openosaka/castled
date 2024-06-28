use std::sync::Arc;
use std::{net::ToSocketAddrs, pin::Pin};

use crate::{manager, Config};
use anyhow::Context as _;
use core::task::{Context, Poll};
use dashmap::DashMap;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{transport::Server as GrpcServer, Request, Response, Status, Streaming};
use tracing::{debug, error, warn};
use tunneld_pkg::event;
use tunneld_protocol::pb::{
    control::Payload,
    tunnel::Config::Tcp,
    tunnel_service_server::{TunnelService, TunnelServiceServer},
    Command, Control, InitPayload, RegisterReq, TrafficToClient, WorkPayload,
};
use tunneld_protocol::pb::{traffic_to_server, TrafficToServer};
use tunneld_protocol::validate::validate_register_req;
use uuid::Uuid;

pub struct Handler {
    event_tx: mpsc::Sender<event::Event>,
    connections: Arc<DashMap<String, event::Connection>>,

    _priv: (),
}

impl Handler {
    pub fn new(event_tx: mpsc::Sender<event::Event>) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            event_tx,
            _priv: (),
        }
    }
}

type GrpcResult<T> = Result<T, Status>;
type GrpcResponse<T> = GrpcResult<Response<T>>;
type RegisterStream = Pin<Box<CancelableReceiver<GrpcResult<Control>>>>;
type DataStream = Pin<Box<dyn Stream<Item = GrpcResult<TrafficToClient>> + Send>>;

pub struct CancelableReceiver<T> {
    cancel: CancellationToken,
    inner: mpsc::Receiver<T>,
}

impl<T> CancelableReceiver<T> {
    pub fn new(cancel: CancellationToken, inner: mpsc::Receiver<T>) -> Self {
        Self { cancel, inner }
    }
}

impl<T> Stream for CancelableReceiver<T> {
    type Item = T;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.inner.poll_recv(cx)
    }
}

impl<T> std::ops::Deref for CancelableReceiver<T> {
    type Target = mpsc::Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> Drop for CancelableReceiver<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub struct Server {
    config: Config,
    server: GrpcServer,
    close: CancellationToken,
}

impl Server {
    /// Create a new server instance.
    pub fn new(config: Config) -> Self {
        let server = GrpcServer::builder();
        let close = CancellationToken::new();

        Self {
            config,
            server,
            close,
        }
    }

    pub async fn run(&mut self, cancel: CancellationToken) -> anyhow::Result<()> {
        let addr = format!("0.0.0.0:{}", self.config.control_port)
            .to_socket_addrs()
            .context("parse control port")?
            .next()
            .context("invalid control_port")
            .unwrap();

        let (event_tx, event_rx) = mpsc::channel(1024);
        let handler = Handler::new(event_tx);
        tokio::spawn(async move {
            manager::TcpManager::new().handle_listeners(event_rx).await;
        });

        let parent_cancel = cancel.clone();
        let server_cancel = self.close.clone();
        tokio::spawn(async move {
            parent_cancel.cancelled().await;
            server_cancel.cancel(); // cancel myself.
        });

        debug!("starting server on: {}", addr);

        self.server
            .add_service(TunnelServiceServer::new(handler))
            .serve_with_shutdown(addr, cancel.cancelled())
            .await?;
        Ok(())
    }
}

#[tonic::async_trait]
impl TunnelService for Handler {
    type RegisterStream = RegisterStream;

    async fn register(&self, req: Request<RegisterReq>) -> GrpcResponse<self::RegisterStream> {
        let req = req.into_inner();
        if let Some(status) = validate_register_req(&req) {
            return Err(status);
        }

        let (tx, rx) = mpsc::channel(1024);
        let tenant_id = Uuid::new_v4();
        let init_command = Control {
            command: Command::Init as i32,
            payload: Some(Payload::Init(InitPayload {
                server_version: "todo".to_string(),
                tunnel_id: tenant_id.to_string(),
                assigned_entrypoint: "todo".to_string(),
            })),
        };
        tx.send(Ok(init_command)).await.map_err(|err| {
            error!("failed to send control stream: {}", err);
            Status::internal("failed to send control stream")
        })?;

        let cancel_listener_w = CancellationToken::new();
        let cancel_listener = cancel_listener_w.clone();
        let event_tx = self.event_tx.clone();

        // TODO(sword): support more tunnel types
        // let's assume it's a TCP tunnel for now
        if let Tcp(tcp) = req.tunnel.as_ref().unwrap().config.as_ref().unwrap() {
            let remote_port = tcp.remote_port.to_owned();
            // notify manager to create a listener,
            let (resp_tx, resp_rx) = oneshot::channel();
            let (new_connection_tx, mut new_connection_rx) =
                mpsc::channel::<event::Connection>(1024);
            event_tx
                .send(event::Event {
                    payload: event::Payload::TcpRegister {
                        port: remote_port as u16,
                        cancel: cancel_listener,
                        new_connection_sender: new_connection_tx,
                    },
                    resp: resp_tx,
                })
                .await
                .context("failed to notify manager to create listener")
                .unwrap();
            match resp_rx.await {
                Ok(None) => {
                    debug!("listener created on port: {}", remote_port);
                }
                Ok(Some(status)) => {
                    tx.send(Err(status)).await.unwrap();
                }
                Err(_err) => {
                    tx.send(Err(Status::internal("failed to create lister")))
                        .await
                        .unwrap();
                }
            }

            let connections = self.connections.clone();
            tokio::spawn(async move {
                // receive new connections
                while let Some(connection) = new_connection_rx.recv().await {
                    let connection_id = connection.id.to_string();
                    connections.insert(connection.id.clone(), connection);
                    tx.send(Ok(Control {
                        command: Command::Work as i32,
                        payload: Some(Payload::Work(WorkPayload { connection_id })),
                    }))
                    .await
                    .context("failed to send work command")
                    .unwrap();
                }
                warn!("todo(sword): release connection");
            });
        } else {
            unimplemented!("only support TCP tunnel for now")
        }

        let control_stream =
            Box::pin(CancelableReceiver::new(cancel_listener_w, rx)) as self::RegisterStream;
        Ok(Response::new(control_stream))
    }

    type DataStream = DataStream;

    async fn data(
        &self,
        req: Request<Streaming<TrafficToServer>>,
    ) -> GrpcResponse<self::DataStream> {
        let connections = self.connections.clone();
        let mut inbound_stream = req.into_inner();
        let (outbound_tx, outbound_rx) = mpsc::channel(1024);

        tokio::spawn(async move {
            let mut started = false;
            while let Some(traffic) = inbound_stream.next().await {
                match traffic {
                    Ok(traffic) => {
                        let connection_id = traffic.connection_id;
                        let connection = connections
                            .get(&connection_id)
                            .context("connection not found")
                            .unwrap();
                        let connection = connection.value();

                        use std::convert::TryFrom;
                        match traffic_to_server::Action::try_from(traffic.action) {
                            Ok(traffic_to_server::Action::Start) => {
                                debug!(
                                    "received start action from connection {}, I am gonna start streaming",
                                    connection_id,
                                );
                                if started {
                                    error!("duplicate start action");
                                    return;
                                }
                                started = true;

                                // we read data from transfer_rx, then forward the data to outbound_tx
                                let (transfer_tx, mut transfer_rx) = mpsc::channel(1024);
                                connection
                                    .channel
                                    .send(event::ConnectionChannelDataType::DataSender(transfer_tx))
                                    .await
                                    .context("failed to send streaming_tx to data_sender_sender")
                                    .unwrap();
                                let outbound_tx = outbound_tx.clone();
                                tokio::spawn(async move {
                                    loop {
                                        tokio::select! {
                                            Some(data) = transfer_rx.recv() => {
                                                outbound_tx
                                                    .send(Ok(TrafficToClient { data }))
                                                    .await
                                                    .context("failed to send traffic to outbound channel")
                                                    .unwrap();
                                            }
                                            _ = outbound_tx.closed() => {
                                                break
                                            }
                                        }
                                    }
                                });
                            }
                            Ok(traffic_to_server::Action::Sending) => {
                                debug!(
                                    "client is sending traffic, connection_id: {}",
                                    connection_id
                                );
                                connection
                                    .channel
                                    .send(event::ConnectionChannelDataType::Data(traffic.data))
                                    .await
                                    .unwrap();
                            }
                            Ok(traffic_to_server::Action::Finished) => {
                                debug!(
                                    "client finished sending traffic, connection_id: {}",
                                    connection_id
                                );
                                connections.remove(&connection_id);
                            }
                            Ok(traffic_to_server::Action::Close) => {
                                debug!("client closed streaming, connection_id: {}", connection_id);
                                // notify tcp manager to close the user connection
                                connection.cancel.cancel();
                                connections.remove(&connection_id);
                                break;
                            }
                            Err(_) => {
                                error!("invalid traffic action: {}", traffic.action);
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        error!("failed to receive traffic: {}", err);
                    }
                }
            }
        });

        let response_streaming = ReceiverStream::new(outbound_rx);
        Ok(Response::new(
            Box::pin(response_streaming) as self::DataStream
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_server() {
        let mut server = Server::new(Config {
            control_port: 8610,
            http_port: 8611,
            domain: "".to_string(),
        });
        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();

        tokio::spawn(async move {
            let result = server.run(cancel).await;
            assert!(result.is_ok());
        });

        sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
