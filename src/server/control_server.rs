use crate::event::ClientEventResponse;
use crate::protocol::pb::{traffic_to_server, TrafficToServer};
use crate::protocol::validate::validate_register_req;
use crate::{bridge, event};
use crate::{
    io::CancellableReceiver,
    protocol::pb::{
        control::Payload,
        tunnel::Config::{Http, Tcp, Udp},
        tunnel_service_server::{TunnelService, TunnelServiceServer},
        Command, Control, InitPayload, RegisterReq, TrafficToClient, WorkPayload,
    },
};
use anyhow::Context as _;
use async_shutdown::{ShutdownManager, ShutdownSignal};
use bytes::Bytes;
use dashmap::DashMap;
use futures::{Future, StreamExt};
use std::sync::Arc;
use std::{net::ToSocketAddrs, pin::Pin};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{transport::Server as GrpcServer, Request, Response, Status, Streaming};
use tracing::{error, info};
use uuid::Uuid;

use super::data_server::DataServer;
use super::Config;

type GrpcResult<T> = Result<T, Status>;
type GrpcResponse<T> = GrpcResult<Response<T>>;
type RegisterStream = Pin<Box<CancellableReceiver<GrpcResult<Control>>>>;
type DataStream = Pin<Box<dyn Stream<Item = GrpcResult<TrafficToClient>> + Send>>;

/// Server is the control server of the tunnel daemon.
///
/// We treat the control server is grpc server as well, in the concept,
/// they are same thing.
/// Although the grpc server provides a [`crate::protocol::pb::tunnel_service_server::TunnelService::data`],
/// it's similar to the data server(a little), but in the `data` function body,
/// the most of work is to forward the data from client to data server.
/// We can understand this is a tunnel between the client and the data server.
pub struct Server {
    /// config is the tcp port of the control server.
    control_port: u16,

    /// control server will give the event to the data server.
    event_rx: mpsc::Receiver<event::ClientEvent>,

    /// control_server is the grpc server instance.
    control_server: GrpcServer,

    /// event_bus is the event bus of the server.
    event_bus: DataServer,

    /// shutdown is the shutdown listener of the server.
    shutdown: ShutdownManager<()>,

    /// handler is the grpc handler of the control server.
    handler: ControlHandler,
}

impl Server {
    /// Create a new server instance.
    pub fn new(config: Config) -> Self {
        let shutdown: ShutdownManager<()> = ShutdownManager::new();
        let (event_tx, event_rx) = mpsc::channel(1024);

        let server = GrpcServer::builder()
            .http2_keepalive_interval(Some(tokio::time::Duration::from_secs(60)))
            .http2_keepalive_timeout(Some(tokio::time::Duration::from_secs(3)));
        let events = DataServer::new(config.vhttp_port, config.entrypoint);
        let handler = ControlHandler::new(shutdown.wait_shutdown_triggered(), event_tx);

        Self {
            control_port: config.control_port,
            control_server: server,
            event_bus: events,
            shutdown,
            handler,
            event_rx,
        }
    }

    /// Run the server, this function blocks on the shutdown future.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// async fn run_server() {
    ///     let server = castled::server::Server::new(castled::server::Config{
    ///         control_port: 8610,
    ///         vhttp_port: 8611,
    ///         ..Default::default()
    ///     });
    ///     let shutdown = tokio::signal::ctrl_c();
    ///     server.run(shutdown).await.unwrap();
    /// }
    /// ```
    pub async fn run(self, shutdown: impl Future) -> anyhow::Result<()> {
        let addr = format!("0.0.0.0:{}", self.control_port)
            .to_socket_addrs()
            .context("parse control port")?
            .next()
            .context("invalid control_port")
            .unwrap();

        let shutdown_listener_control_server = self.shutdown.wait_shutdown_triggered();
        let shutdown_listener_event_bus = self.shutdown.wait_shutdown_triggered();

        let event_bus = self.event_bus;
        tokio::spawn(async move {
            event_bus
                .listen(shutdown_listener_event_bus, self.event_rx)
                .await
        });

        info!("starting control server on: {}", addr);

        let mut control_server = self.control_server;
        let run_control_server = async move {
            control_server
                .add_service(TunnelServiceServer::new(self.handler))
                .serve_with_shutdown(addr, async {
                    shutdown_listener_control_server.await;
                })
                .await
        };

        tokio::select! {
            _ = shutdown => {
                info!("shutdown signal received, shutting down the server");
            }
            res = run_control_server => {
                if let Err(err) = res {
                    error!("server quit: {}", err);
                }
            }
        }

        self.shutdown.trigger_shutdown(())?;

        Ok(())
    }
}

/// ControlHeader is the core of the control server,
/// it implements the grpc TunnelService.
struct ControlHandler {
    event_tx: mpsc::Sender<event::ClientEvent>,
    bridges: Arc<DashMap<Bytes, bridge::DataSenderBridge>>,
    close_sender_notifiers: Arc<DashMap<Bytes, CancellationToken>>,
    shutdown: ShutdownSignal<()>,
}

impl ControlHandler {
    fn new(shutdown: ShutdownSignal<()>, event_tx: mpsc::Sender<event::ClientEvent>) -> Self {
        Self {
            bridges: Arc::new(DashMap::new()),
            close_sender_notifiers: Arc::new(DashMap::new()),
            event_tx,
            shutdown,
        }
    }
}

#[tonic::async_trait]
impl TunnelService for ControlHandler {
    type RegisterStream = RegisterStream;

    async fn register(&self, req: Request<RegisterReq>) -> GrpcResponse<self::RegisterStream> {
        let req = req.into_inner();
        if let Some(status) = validate_register_req(&req) {
            return Err(status);
        }

        let (outbound_streaming_tx, outbound_streaming_rx) = mpsc::channel(256);
        let outbound_streaming_tx_init_message = outbound_streaming_tx.clone();
        let (entrypoint_tx, entrypoint_rx) = oneshot::channel();
        tokio::spawn(async move {
            // wait for the entrypoint assigned by the server
            let tenant_id = Uuid::new_v4();
            let init_command = Control {
                command: Command::Init as i32,
                payload: Some(Payload::Init(InitPayload {
                    tunnel_id: tenant_id.to_string(),
                    assigned_entrypoint: entrypoint_rx.await.unwrap(),
                })),
            };
            outbound_streaming_tx_init_message
                .send(Ok(init_command))
                .await
                .unwrap();
        });

        let register_cancel = CancellationToken::new();
        let event_tx = self.event_tx.clone();

        let (resp_tx, resp_rx) = oneshot::channel();
        let (user_incoming_tx, mut user_incoming_rx) = mpsc::channel::<event::UserIncoming>(1024);

        match req.tunnel.as_ref().unwrap().config.as_ref().unwrap() {
            Tcp(tcp) => {
                info!(
                    remote_port = tcp.remote_port,
                    "registering tcp tunnel on remote_port"
                );
                event_tx
                    .send(event::ClientEvent {
                        payload: event::Payload::RegisterTcp {
                            port: tcp.remote_port as u16,
                        },
                        close_listener: register_cancel.clone(),
                        incoming_events: user_incoming_tx,
                        resp: resp_tx,
                    })
                    .await
                    .map_err(|err| {
                        error!(err = ?err, "failed to register tcp tunnel");
                        Status::internal("failed to register tcp tunnel")
                    })?;
            }
            Http(http) => {
                event_tx
                    .send(event::ClientEvent {
                        payload: event::Payload::RegisterHttp {
                            port: http.remote_port as u16,
                            subdomain: Bytes::from(http.subdomain.to_owned()),
                            domain: Bytes::from(http.domain.to_owned()),
                            random_subdomain: http.random_subdomain,
                        },
                        close_listener: register_cancel.clone(),
                        incoming_events: user_incoming_tx,
                        resp: resp_tx,
                    })
                    .await
                    .map_err(|err| {
                        error!(err = ?err, "failed to register http tunnel");
                        Status::internal("failed to register http tunnel")
                    })?;
            }
            Udp(udp) => {
                event_tx
                    .send(event::ClientEvent {
                        payload: event::Payload::RegisterUdp {
                            port: udp.remote_port as u16,
                        },
                        close_listener: register_cancel.clone(),
                        incoming_events: user_incoming_tx,
                        resp: resp_tx,
                    })
                    .await
                    .map_err(|err| {
                        error!(err = ?err, "failed to register udp tunnel");
                        Status::internal("failed to register udp tunnel")
                    })?;
            }
        }

        match resp_rx.await {
            Ok(ClientEventResponse::Registered { status, entrypoint }) => match status {
                None => {
                    entrypoint_tx.send(entrypoint).unwrap();
                }
                Some(status) => {
                    outbound_streaming_tx.send(Err(status)).await.unwrap();
                }
            },
            Err(err) => {
                error!(err = ?err, "failed to send response to client, the connection may be closed");
                if let Err(err) = outbound_streaming_tx
                    .send(Err(Status::internal("failed to create listener")))
                    .await
                {
                    error!(err = ?err, "failed to respond register request");
                }
            }
        }

        let shutdown_listener = self.shutdown.clone();
        let bridges = self.bridges.clone();
        let register_cancel_listener = register_cancel.clone();
        let close_sender_notifiers = Arc::clone(&self.close_sender_notifiers);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_listener.clone() => {
                        info!("server closed, close the control stream");
                        return;
                    }
                    _ = register_cancel_listener.cancelled() => {
                        info!("register cancelled, close the control stream");
                        return;
                    }
                    Some(connection) = user_incoming_rx.recv() => {
                        match connection {
                            event::UserIncoming::Add(bridge) => {
                                let bridge_id = String::from_utf8_lossy(bridge.id.to_vec().as_slice()).to_string();
                                info!(bridge_id = bridge_id, "new user connection");
                                bridges.insert(bridge.id, bridge.inner);
                                outbound_streaming_tx
                                    .send(Ok(Control {
                                        command: Command::Work as i32,
                                        payload: Some(Payload::Work(WorkPayload { connection_id: bridge_id })),
                                    }))
                                    .await
                                    .context("failed to send work command")
                                    .unwrap();
                            }
                            event::UserIncoming::Remove(bridge_id) => {
                                {
                                    let bridge_id = String::from_utf8_lossy(bridge_id.to_vec().as_slice()).to_string();
                                    info!(bridge_id = bridge_id, "remove user connection");
                                }
                                bridges.remove(&bridge_id);
                                close_sender_notifiers.remove(&bridge_id).unwrap().1.cancel();
                            }
                        }
                    }
                }
            }
        });

        let control_stream = Box::pin(CancellableReceiver::new(
            register_cancel,
            outbound_streaming_rx,
        )) as self::RegisterStream;
        Ok(Response::new(control_stream))
    }

    type DataStream = DataStream;

    /// data implements the grpc [`crate::protocol::pb::tunnel_service_server::TunnelService::data`].
    ///
    /// The data function is the core of the control server,
    /// it forwards the data between the client and the data server.
    async fn data(
        &self,
        req: Request<Streaming<TrafficToServer>>,
    ) -> GrpcResponse<self::DataStream> {
        let bridges = self.bridges.clone();
        let mut inbound_stream = req.into_inner();
        let (outbound_tx, outbound_rx) = mpsc::channel(256);

        let shutdown_listener = self.shutdown.clone();
        let close_sender_notifiers = Arc::clone(&self.close_sender_notifiers);
        tokio::spawn(async move {
            let mut stream_started = false;
            loop {
                tokio::select! {
                    _ = shutdown_listener.clone() => { break }
                    Some(traffic) = inbound_stream.next() => {
                        match traffic {
                            Ok(traffic) => {
                                let bridge_id_str = traffic.connection_id;
                                let bridge_id = Bytes::copy_from_slice(bridge_id_str.as_bytes());
                                let bridge = bridges
                                    .get(&bridge_id)
                                    .context("connection not found")
                                    .unwrap();
                                let bridge = bridge.value();

                                match traffic_to_server::Action::try_from(traffic.action) {
                                    Ok(traffic_to_server::Action::Start) => {
                                        info!(
                                            bridge_id = bridge_id_str,
                                            "received start action, I am gonna start streaming",
                                        );
                                        if stream_started {
                                            error!("duplicate start action");
                                            return;
                                        }
                                        stream_started = true;

                                        let close_sender = CancellationToken::new();
                                        let close_sender_listener = close_sender.clone();
                                        close_sender_notifiers.insert(bridge_id, close_sender);

                                        // we read data from transfer_rx, then forward the data to outbound_tx
                                        let (transfer_tx, mut transfer_rx) = mpsc::channel(256);
                                        bridge.send_sender(transfer_tx)
                                            .await
                                            .context("failed to send streaming_tx to data_sender_sender")
                                            .unwrap();
                                        let outbound_tx = outbound_tx.clone();
                                        tokio::spawn(async move {
                                            loop {
                                                tokio::select! {
                                                    // server -> client
                                                    Some(data) = transfer_rx.recv() => {
                                                        outbound_tx
                                                            .send(Ok(TrafficToClient { data }))
                                                            .await
                                                            .context("failed to send traffic to outbound channel")
                                                            .unwrap();
                                                    }
                                                    _ = close_sender_listener.cancelled() => {
                                                        // after connection is removed, this listener will be notified
                                                        return;
                                                    }
                                                    _ = outbound_tx.closed() => {
                                                        // when the client is closed, the server will be notified,
                                                        // because the outbound_rx will be dropped.
                                                        return;
                                                    }
                                                }
                                            }
                                        });
                                    }
                                    Ok(traffic_to_server::Action::Sending) => {
                                        info!(
                                            bridge_id = bridge_id_str,
                                            "client is sending traffic",
                                        );
                                        // client -> server
                                        bridge.send_data(traffic.data).await.unwrap();
                                    }
                                    Ok(traffic_to_server::Action::Finished) => {
                                        info!(
                                            bridge_id = bridge_id_str,
                                            "client finished sending traffic",
                                        );
                                        bridge.send_data(vec![]).await.unwrap();
                                        return; // close the data streaming
                                    }
                                    Ok(traffic_to_server::Action::Close) => {
                                        info!(bridge_id = bridge_id_str, "client closed streaming");
                                        bridge.close();
                                        return; // close the data streaming
                                    }
                                    Err(_) => {
                                        error!("invalid traffic action: {}", traffic.action);
                                        bridge.close();
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
    use crate::server::EntrypointConfig;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_server() {
        let server = Server::new(Config {
            control_port: 8610,
            vhttp_port: 8611,
            entrypoint: EntrypointConfig {
                domain: vec!["example.com".to_string()],
                ..Default::default()
            },
        });
        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();

        tokio::spawn(async move {
            let result = server.run(cancel.cancelled()).await;
            assert!(result.is_ok());
        });

        sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
