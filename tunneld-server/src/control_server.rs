use crate::data_server::DataServer;
use anyhow::Context as _;
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
use tunneld_pkg::bridge;
use tunneld_pkg::event;
use tunneld_pkg::io::CancellableReceiver;
use tunneld_pkg::shutdown::{self, ShutdownListener};
use tunneld_protocol::pb::{
    control::Payload,
    tunnel::Config::Http,
    tunnel::Config::Tcp,
    tunnel::Config::Udp,
    tunnel_service_server::{TunnelService, TunnelServiceServer},
    Command, Control, InitPayload, RegisterReq, TrafficToClient, WorkPayload,
};
use tunneld_protocol::pb::{traffic_to_server, TrafficToServer};
use tunneld_protocol::validate::validate_register_req;
use uuid::Uuid;

type GrpcResult<T> = Result<T, Status>;
type GrpcResponse<T> = GrpcResult<Response<T>>;
type RegisterStream = Pin<Box<CancellableReceiver<GrpcResult<Control>>>>;
type DataStream = Pin<Box<dyn Stream<Item = GrpcResult<TrafficToClient>> + Send>>;

/// Server is the control server of the tunnel daemon.
///
/// We treat the control server is grpc server as well, in the concept,
/// they are same thing.
/// Although the grpc server provides a [`tunneld_protocol::pb::tunnel_service_server::TunnelService::data`],
/// it's similar to the data server(a little), but in the `data` function body,
/// the most of work is to forward the data from client to data server.
/// We can understand this is a tunnel between the client and the data server.
pub struct Server {
    /// control_port is the port of the control server.
    ///
    /// the client will connect to this port to register a tunnel.
    control_port: u16,

    /// control_server is the grpc server instance.
    control_server: GrpcServer,

    /// event_bus is the event bus of the server.
    event_bus: DataServer,

    /// shutdown is the shutdown listener of the server.
    /// when the shutdown signal is received, shutdown the server
    /// by [`tunneld_pkg::shutdown::Shutdown::notify`].
    shutdown: shutdown::Shutdown,
}

impl Server {
    /// Create a new server instance.
    pub fn new(control_port: u16, vhttp_port: u16, domain: String) -> Self {
        let server = GrpcServer::builder()
            .http2_keepalive_interval(Some(tokio::time::Duration::from_secs(60)))
            .http2_keepalive_timeout(Some(tokio::time::Duration::from_secs(3)));
        let events = DataServer::new(vhttp_port, domain);

        Self {
            control_port,
            control_server: server,
            event_bus: events,
            shutdown: shutdown::Shutdown::new(),
        }
    }

    /// Run the server, this function blocks on the shutdown future.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// async fn run_server() {
    ///     let server = tunneld_server::Server::new(6610, 6611, String::from("example.com"));
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

        let (event_tx, event_rx) = mpsc::channel(1024);

        let shutdown_listener_control_server = self.shutdown.listen();
        let shutdown_listener_event_bus = self.shutdown.listen();

        let handler = ControlHandler::new(self.shutdown.listen(), event_tx);
        let event_bus = self.event_bus;
        tokio::spawn(async move {
            event_bus
                .listen(shutdown_listener_event_bus, event_rx)
                .await
        });

        info!("starting control server on: {}", addr);

        let mut control_server = self.control_server;
        let run_control_server = async move {
            control_server
                .add_service(TunnelServiceServer::new(handler))
                .serve_with_shutdown(addr, shutdown_listener_control_server)
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

        // when close shutdown_listener, the control server will start to shutdown.
        self.shutdown.notify();

        Ok(())
    }
}

/// ControlHeader is the core of the control server,
/// it implements the grpc TunnelService.
struct ControlHandler {
    event_tx: mpsc::Sender<event::ClientEvent>,
    bridges: Arc<DashMap<Bytes, bridge::DataSenderBridge>>,
    close_sender_notifiers: Arc<DashMap<Bytes, CancellationToken>>,
    shutdown: ShutdownListener,
}

impl ControlHandler {
    fn new(shutdown: ShutdownListener, event_tx: mpsc::Sender<event::ClientEvent>) -> Self {
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
        let tenant_id = Uuid::new_v4();
        let init_command = Control {
            command: Command::Init as i32,
            payload: Some(Payload::Init(InitPayload {
                tunnel_id: tenant_id.to_string(),
                assigned_entrypoint: "todo".to_string(),
            })),
        };
        outbound_streaming_tx
            .send(Ok(init_command))
            .await
            .map_err(|err| {
                error!(err = ?err, "failed to send control stream");
                Status::internal("failed to send control stream")
            })?;

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
                let remote_port = tcp.remote_port.to_owned();
                event_tx
                    .send(event::ClientEvent {
                        payload: event::Payload::RegisterTcp {
                            port: remote_port as u16,
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
            Ok(None) => {}
            Ok(Some(status)) => {
                outbound_streaming_tx.send(Err(status)).await.unwrap();
            }
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
                    _ = shutdown_listener.done() => {
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

    /// data implements the grpc [`tunneld_protocol::pb::tunnel_service_server::TunnelService::data`].
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
                    _ = shutdown_listener.done() => { break }
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
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_server() {
        let server = Server::new(8610, 8611, "".to_string());
        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();

        tokio::spawn(async move {
            let result = server.run(cancel.cancelled()).await;
            assert!(result.is_ok());
        });

        sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
