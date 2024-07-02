use bytes::Bytes;
use std::sync::Arc;
use std::{net::ToSocketAddrs, pin::Pin};

use crate::transport::EventBus;
use crate::Config;
use anyhow::Context as _;
use core::task::{Context, Poll};
use dashmap::DashMap;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{transport::Server as GrpcServer, Request, Response, Status, Streaming};
use tracing::{debug, error, info};
use tunneld_pkg::event;
use tunneld_protocol::pb::{
    control::Payload,
    tunnel::Config::Http,
    tunnel::Config::Tcp,
    tunnel_service_server::{TunnelService, TunnelServiceServer},
    Command, Control, InitPayload, RegisterReq, TrafficToClient, WorkPayload,
};
use tunneld_protocol::pb::{traffic_to_server, TrafficToServer};
use tunneld_protocol::validate::validate_register_req;
use uuid::Uuid;

pub struct Handler {
    event_tx: mpsc::Sender<event::Event>,
    connections: Arc<DashMap<String, event::Conn>>,
    close: CancellationToken,

    _priv: (),
}

impl Handler {
    pub fn new(close: CancellationToken, event_tx: mpsc::Sender<event::Event>) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            event_tx,
            close,
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
    control_port: u16,
    _vhttp_port: u16,
    control_server: GrpcServer,
    events: EventBus,
}

impl Server {
    /// Create a new server instance.
    pub fn new(config: Config) -> Self {
        let server = GrpcServer::builder()
            .http2_keepalive_interval(Some(tokio::time::Duration::from_secs(60)))
            .http2_keepalive_timeout(Some(tokio::time::Duration::from_secs(3)));
        let events = EventBus::new(config.vhttp_port, config.domain.clone());

        Self {
            control_port: config.control_port,
            _vhttp_port: config.vhttp_port,
            control_server: server,
            events,
        }
    }

    pub async fn run(self, cancel: CancellationToken) -> anyhow::Result<()> {
        let addr = format!("0.0.0.0:{}", self.control_port)
            .to_socket_addrs()
            .context("parse control port")?
            .next()
            .context("invalid control_port")
            .unwrap();

        let (event_tx, event_rx) = mpsc::channel(1024);

        let handler = Handler::new(cancel.clone(), event_tx);
        let events = self.events;
        tokio::spawn(async move {
            events.listen(event_rx).await;
        });

        info!("starting control server on: {}", addr);

        let mut control_server = self.control_server;
        let run_control_server = tokio::spawn(async move {
            control_server
                .add_service(TunnelServiceServer::new(handler))
                .serve_with_shutdown(addr, cancel.cancelled())
                .await
        });

        let _grpc_result = tokio::join!(run_control_server);
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

        let (outbound_streaming_tx, outbound_streaming_rx) = mpsc::channel(1024);
        let tenant_id = Uuid::new_v4();
        let init_command = Control {
            command: Command::Init as i32,
            payload: Some(Payload::Init(InitPayload {
                server_version: "todo".to_string(),
                tunnel_id: tenant_id.to_string(),
                assigned_entrypoint: "todo".to_string(),
            })),
        };
        outbound_streaming_tx
            .send(Ok(init_command))
            .await
            .map_err(|err| {
                error!("failed to send control stream: {}", err);
                Status::internal("failed to send control stream")
            })?;

        let cancel_listener_w = CancellationToken::new();
        let cancel_listener = cancel_listener_w.clone();
        let event_tx = self.event_tx.clone();
        let server_closed = self.close.clone();

        let (resp_tx, resp_rx) = oneshot::channel();
        let (conn_event_chan_tx, mut conn_event_chan_rx) = mpsc::channel::<event::ConnEvent>(1024);

        match req.tunnel.as_ref().unwrap().config.as_ref().unwrap() {
            Tcp(tcp) => {
                debug!("registering tcp tunnel on remote_port: {:?}", tcp.remote_port);
                let remote_port = tcp.remote_port.to_owned();
                event_tx
                    .send(event::Event {
                        payload: event::Payload::RegisterTcp {
                            port: remote_port as u16,
                        },
                        cancel: cancel_listener,
                        conn_event_chan: conn_event_chan_tx,
                        resp: resp_tx,
                    })
                    .await
                    .context("failed to register tcp tunnel")
                    .unwrap();
            }
            Http(http) => {
                event_tx
                    .send(event::Event {
                        payload: event::Payload::RegisterHttp {
                            port: http.remote_port as u16,
                            subdomain: Bytes::from(http.subdomain.to_owned()),
                            domain: Bytes::from(http.domain.to_owned()),
                        },
                        cancel: cancel_listener,
                        conn_event_chan: conn_event_chan_tx,
                        resp: resp_tx,
                    })
                    .await
                    .context("failed to register http tunnel")
                    .unwrap();
            }
        }

        match resp_rx.await {
            Ok(None) => {}
            Ok(Some(status)) => {
                outbound_streaming_tx.send(Err(status)).await.unwrap();
            }
            Err(err) => {
                error!("register response: {:?}", err);
                outbound_streaming_tx
                    .send(Err(Status::internal("failed to create listener")))
                    .await
                    .unwrap();
            }
        }

        let connections = self.connections.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = server_closed.cancelled() => {
                        debug!("server closed, close the control stream");
                        break;
                    }
                    Some(connection) = conn_event_chan_rx.recv() => {
                        match connection {
                            event::ConnEvent::Add(connection) => {
                                debug!("new user connection {}", connection.id);
                                // receive new connection
                                let connection_id = connection.id.to_string();
                                connections.insert(connection.id.clone(), connection);
                                outbound_streaming_tx
                                    .send(Ok(Control {
                                        command: Command::Work as i32,
                                        payload: Some(Payload::Work(WorkPayload { connection_id })),
                                    }))
                                    .await
                                    .context("failed to send work command")
                                    .unwrap();
                            }
                            event::ConnEvent::Remove(connection_id) => {
                                debug!("remove user connection: {}", connection_id);
                                connections.remove(&connection_id);
                            }
                        }
                    }
                }
            }
        });

        let control_stream = Box::pin(CancelableReceiver::new(
            cancel_listener_w,
            outbound_streaming_rx,
        )) as self::RegisterStream;
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

        let server_closed = self.close.clone();
        tokio::spawn(async move {
            let mut started = false;
            loop {
                tokio::select! {
                    _ = server_closed.cancelled() => { break }
                    Some(traffic) = inbound_stream.next() => {
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
                                            .chan
                                            .send(event::ConnChanDataType::DataSender(transfer_tx))
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
                                            .chan
                                            .send(event::ConnChanDataType::Data(traffic.data))
                                            .await
                                            .unwrap();
                                    }
                                    Ok(traffic_to_server::Action::Finished) => {
                                        debug!(
                                            "client finished sending traffic, connection_id: {}",
                                            connection_id
                                        );
                                        connection.chan.send(event::ConnChanDataType::Data(vec![])).await.unwrap();
                                    }
                                    Ok(traffic_to_server::Action::Close) => {
                                        debug!("client closed streaming, connection_id: {}", connection_id);
                                        // notify tcp manager to close the user connection
                                        connection.cancel.cancel();
                                    }
                                    Err(_) => {
                                        error!("invalid traffic action: {}", traffic.action);
                                        connection.cancel.cancel();
                                    }
                                }
                            }
                            Err(err) => {
                                error!("failed to receive traffic: {}", err);
                            }
                        }
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
        let server = Server::new(Config {
            control_port: 8610,
            vhttp_port: 8611,
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
