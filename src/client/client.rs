use anyhow::{Context, Result};
use async_shutdown::{ShutdownManager, ShutdownSignal};
use std::net::SocketAddr;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{transport::Channel, Response, Status, Streaming};
use tracing::{debug, error, info, instrument, span};

use tokio::{
    io::{self, AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    select,
    sync::{mpsc, oneshot},
};

use crate::{
    io::{StreamingReader, StreamingWriter, TrafficToServerWrapper},
    pb::{
        self, control::Payload, traffic_to_server, tunnel::Type,
        tunnel_service_client::TunnelServiceClient, Command, Control, RegisterReq, TrafficToClient,
        TrafficToServer,
    },
};

use super::tunnel::Tunnel;

/// Client represents a castle client that can register tunnels with the server.
#[derive(Clone)]
pub struct Client {
    control_addr: SocketAddr,
}

impl Client {
    /// Creates a new `Client` instance with the specified control address.
    ///
    /// ```
    /// let client = castled::client::Client::new("127.0.0.1:6100".parse().unwrap());
    /// ```
    pub fn new(addr: SocketAddr) -> Self {
        Self { control_addr: addr }
    }

    /// Registers a tunnel with the server and returns a future that represents the tunnel handler.
    /// Also returns a receiver for receiving the assigned entrypoint from the server.
    ///
    /// ```no_run
    /// use std::net::SocketAddr;
    /// use castled::client::{
    ///     Client,
    ///     tunnel::new_tcp_tunnel,
    /// };
    /// use async_shutdown::ShutdownManager;
    ///
    /// async fn run() {
    ///     let client = Client::new("127.0.0.1:6100".parse().unwrap());
    ///     let tunnel = new_tcp_tunnel("my-tunnel", SocketAddr::from(([127, 0, 0, 1], 8971)), 8080);
    ///     let shutdown = ShutdownManager::new();
    ///     let entrypoint = client.start_tunnel(tunnel, shutdown.clone()).await.unwrap();
    ///     println!("entrypoint: {:?}", entrypoint);
    ///     shutdown.wait_shutdown_complete().await;
    /// }
    /// ```
    pub async fn start_tunnel(
        self,
        tunnel: Tunnel,
        shutdown: ShutdownManager<i8>,
    ) -> Result<Vec<String>> {
        let (entrypoint_tx, entrypoint_rx) = oneshot::channel();

        tokio::spawn(async move {
            let run_tunnel = self.handle_tunnel(
                shutdown.wait_shutdown_triggered(),
                tunnel,
                Some(move |entrypoint| {
                    let _ = entrypoint_tx.send(entrypoint);
                }),
            );

            tokio::select! {
                _ = shutdown.wait_shutdown_triggered() => {
                    debug!("cancelling tcp tunnel");
                }
                result = run_tunnel =>  match result {
                    Ok(_) => {
                        info!("tunnel closed");
                    },
                    Err(err) => {
                        error!(err = ?err, "tunnel closed unexpectedly");
                        return shutdown.trigger_shutdown_token(1);
                    },
                }
            }
            shutdown.trigger_shutdown_token(0)
        });

        let entrypoint = entrypoint_rx
            .await
            .context("failed to start tunnel, check the log")?;

        Ok(entrypoint)
    }

    /// wait to receive first init command from the server.
    /// we treat the tunnel has been established successfully after we receive the init command.
    async fn wait_until_registered(
        &self,
        shutdown: ShutdownSignal<i8>,
        control_stream: &mut Streaming<Control>,
    ) -> Result<Vec<String>> {
        select! {
            _ = shutdown => {
                debug!("cancelling tcp tunnel");
                return Err(Status::cancelled("tunnel registration cancelled").into());
            }
            result = control_stream.next() => {
                let result = result.unwrap();
                if result.is_err() {
                    return Err(result.unwrap_err().into());
                }
                let control = result.unwrap();
                match Command::try_from(control.command) {
                    Ok(Command::Init) => {
                        match control.payload {
                            Some(Payload::Init(init)) => {
                                info!(
                                    tunnel_id = init.tunnel_id,
                                    entrypoint = ?init.assigned_entrypoint,
                                    "tunnel registered successfully",
                                );
                                return Ok(init.assigned_entrypoint);
                            }
                            Some(Payload::Work(_)) => {
                                error!("unexpected work command");
                            }
                            None => {
                                error!("missing payload in init command");
                            }
                        }
                    },
                    Ok(cmd) => {
                        error!(cmd = ?cmd, "unexpected command");
                    },
                    Err(err) => {
                        error!(err = ?err, "unexpected command");
                    },
                }
            }
        }
        Err(anyhow::anyhow!("failed to register tunnel"))
    }

    async fn register_tunnel(
        &self,
        rpc_client: &mut TunnelServiceClient<Channel>,
        tunnel: pb::Tunnel,
    ) -> Result<Response<Streaming<Control>>> {
        let span = span!(
            tracing::Level::INFO,
            "register_tunnel",
            tunnel_name = tunnel.name
        );
        let _enter = span.enter();

        rpc_client
            .register(RegisterReq {
                tunnel: Some(tunnel),
            })
            .await
            .map_err(|err| err.into())
    }

    /// Handles the tunnel registration process.
    #[allow(dead_code)]
    async fn handle_tunnel(
        &self,
        shutdown: ShutdownSignal<i8>,
        tunnel: Tunnel,
        hook: Option<impl FnOnce(Vec<String>) + Send + 'static>,
    ) -> Result<()> {
        let mut rpc_client = self.new_rpc_client().await?;
        let is_udp = tunnel.inner.r#type == Type::Udp as i32;
        let register = self.register_tunnel(&mut rpc_client, tunnel.inner);

        tokio::select! {
            _ = shutdown.clone() => {
                Ok(())
            }
            register_resp = register => {
                let register_resp = register_resp?;
                self.handle_control_stream(
                    shutdown.clone(),
                    rpc_client,
                    register_resp,
                    tunnel.local_endpoint,
                    is_udp,
                    hook,
                ).await
            }
        }
    }

    /// Handles the control stream from the server.
    #[instrument(skip(self, shutdown, rpc_client, register_resp, hook))]
    async fn handle_control_stream(
        &self,
        shutdown: ShutdownSignal<i8>,
        rpc_client: TunnelServiceClient<Channel>,
        register_resp: tonic::Response<Streaming<Control>>,
        local_endpoint: SocketAddr,
        is_udp: bool,
        mut hook: Option<impl FnOnce(Vec<String>) + Send + 'static>,
    ) -> Result<()> {
        let mut control_stream = register_resp.into_inner();

        let entrypoint = self
            .wait_until_registered(shutdown.clone(), &mut control_stream)
            .await?;
        if let Some(hook) = hook.take() {
            hook(entrypoint);
        }

        self.start_streaming(
            shutdown,
            &mut control_stream,
            rpc_client,
            local_endpoint,
            is_udp,
        )
        .await
    }

    async fn start_streaming(
        &self,
        shutdown: ShutdownSignal<i8>,
        control_stream: &mut Streaming<Control>,
        rpc_client: TunnelServiceClient<Channel>,
        local_endpoint: SocketAddr,
        is_udp: bool,
    ) -> Result<()> {
        loop {
            tokio::select! {
                result = control_stream.next() => {
                    if result.is_none() {
                        debug!("control stream closed");
                        break;
                    }
                    let result = result.unwrap();
                    if result.is_err() {
                        return Err(result.unwrap_err().into());
                    }
                    let control = result.unwrap();
                    match Command::try_from(control.command) {
                        Ok(Command::Init) => {
                            error!("tunnel has been initialized, should not receive init command again");
                            return Err(anyhow::anyhow!("tunnel has been initialized"));
                        },
                        Ok(Command::Work) => {
                            match control.payload {
                                Some(Payload::Init(_)) => {
                                    error!("unexpected init command");
                                }
                                Some(Payload::Work(work)) => {
                                    debug!("received work command, starting to forward traffic");
                                    if let Err(err) = handle_work_traffic(
                                        rpc_client.clone() /* cheap clone operation */,
                                        &work.connection_id,
                                        local_endpoint,
                                        is_udp,
                                    ).await {
                                        error!(err = ?err, "failed to handle work traffic");
                                    } else {
                                        continue; // the only path to success.
                                    }
                                }
                                None => {
                                    error!("missing payload in work command");
                                    return Err(anyhow::anyhow!("missing payload in work command"));
                                }
                            }
                            break;
                        },
                        _ => {
                            error!(command = %control.command, "unexpected command");
                            return Err(anyhow::anyhow!("unexpected command"));
                        }
                    }
                }
                _ = shutdown.clone() => {
                    debug!("cancelling tcp tunnel");
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Creates a new RPC client and connects to the server.
    #[instrument(skip(self))]
    async fn new_rpc_client(&self) -> Result<TunnelServiceClient<Channel>> {
        debug!("connecting server");

        TunnelServiceClient::connect(format!("http://{}", self.control_addr))
            .await
            .context("Failed to connect to the server")
            .map_err(Into::into)
    }
}

/// Handles the work traffic from the server to the local endpoint.
#[instrument(skip(rpc_client))]
async fn handle_work_traffic(
    mut rpc_client: TunnelServiceClient<Channel>,
    connection_id: &str,
    local_endpoint: SocketAddr,
    is_udp: bool,
) -> Result<()> {
    // write response to the streaming_tx
    // rpc_client sends the data from reading the streaming_rx
    let (streaming_tx, streaming_rx) = mpsc::channel::<TrafficToServer>(64);
    let streaming_to_server = ReceiverStream::new(streaming_rx);
    let connection_id = connection_id.to_string();

    // the first message to notify the server this connection is started to send data
    streaming_tx
        .send(TrafficToServer {
            connection_id: connection_id.clone(),
            action: traffic_to_server::Action::Start as i32,
            ..Default::default()
        })
        .await
        .unwrap();

    // write the data streaming response to transfer_tx,
    // then forward_traffic_to_local can read the data from transfer_rx
    let (transfer_tx, mut transfer_rx) = mpsc::channel::<TrafficToClient>(64);

    let (local_conn_established_tx, local_conn_established_rx) = mpsc::channel::<()>(1);
    let mut local_conn_established_rx = Some(local_conn_established_rx);
    tokio::spawn(async move {
        let mut streaming_response = rpc_client
            .data(streaming_to_server)
            .await
            .unwrap()
            .into_inner();

        let mut local_conn_established = false;
        loop {
            let result = streaming_response.next().await;

            // if the local connection is not established, we should wait until it's established
            if !local_conn_established {
                if let None = local_conn_established_rx.take().unwrap().recv().await {
                    debug!("connecting to local endpoint failed");
                    return;
                } else {
                    info!("local connection established");
                    local_conn_established = true;
                }
            }

            match result {
                Some(Ok(traffic)) => {
                    transfer_tx.send(traffic).await.unwrap();
                }
                Some(Err(status)) => {
                    error!("received error status: {:?}", status);
                    return;
                }
                None => {
                    // when the server finished traffic, it will close the data streaming
                    debug!("data streaming closed by the server");
                    return;
                }
            }
        }
    });

    let wrapper = TrafficToServerWrapper::new(connection_id.clone());
    let mut writer = StreamingWriter::new(streaming_tx.clone(), wrapper);

    if is_udp {
        tokio::spawn(async move {
            let local_addr: SocketAddr = if local_endpoint.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            }
            .parse()
            .unwrap();
            let socket = UdpSocket::bind(local_addr).await;
            if socket.is_err() {
                error!(err = ?socket.err(), "failed to init udp socket, so let's notify the server to close the user connection");

                streaming_tx
                    .send(TrafficToServer {
                        connection_id: connection_id.to_string(),
                        action: traffic_to_server::Action::Close as i32,
                        ..Default::default()
                    })
                    .await
                    .context("terrible, the server may be crashed")
                    .unwrap();
                return;
            }
            let socket = socket.unwrap();
            let result = socket.connect(local_endpoint).await;
            if let Err(err) = result {
                error!(err = ?err, "failed to connect to local endpoint, so let's notify the server to close the user connection");
            }
            local_conn_established_tx.send(()).await.unwrap();

            let buf = transfer_rx.recv().await.unwrap();
            socket.send(&buf.data).await.unwrap();
            let mut buf = vec![0u8; 65507];
            let n = socket.recv(&mut buf).await;
            writer.write_all(&buf[..n.unwrap()]).await.unwrap();
            writer.shutdown().await.unwrap();
        });
    } else {
        tokio::spawn(async move {
            let local_conn = TcpStream::connect(local_endpoint).await;
            if local_conn.is_err() {
                error!("failed to connect to local endpoint {}, so let's notify the server to close the user connection", local_endpoint);

                streaming_tx
                    .send(TrafficToServer {
                        connection_id: connection_id.to_string(),
                        action: traffic_to_server::Action::Close as i32,
                        ..Default::default()
                    })
                    .await
                    .context("terrible, the server may be crashed")
                    .unwrap();
                return;
            }

            let mut local_conn = local_conn.unwrap();
            let (local_r, local_w) = local_conn.split();
            local_conn_established_tx.send(()).await.unwrap();

            if let Err(err) = forward_traffic_to_local(
                local_r,
                local_w,
                StreamingReader::new(transfer_rx),
                writer,
            )
            .await
            {
                debug!("failed to forward traffic to local: {:?}", err);
            }
        });
    }

    Ok(())
}

/// Forwards the traffic from the server to the local endpoint.
///
/// Try to imagine the current client is yourself,
/// your mission is to forward the traffic from the server to the local,
/// then write the original response back to the server.
/// in this process, there are two underlying connections:
/// 1. remote <=> me
/// 2. me     <=> local
async fn forward_traffic_to_local(
    mut local_r: impl AsyncRead + Unpin,
    mut local_w: impl AsyncWrite + Unpin,
    mut remote_r: StreamingReader<TrafficToClient>,
    mut remote_w: StreamingWriter<TrafficToServer>,
) -> Result<()> {
    let remote_to_me_to_local = async {
        // read from remote, write to local
        match io::copy(&mut remote_r, &mut local_w).await {
            Ok(n) => {
                debug!("copied {} bytes from remote to local", n);
                let _ = local_w.shutdown().await;
            }
            Err(err) => {
                error!(err = ?err, "failed to copy from remote to local");
            }
        }
    };

    let local_to_me_to_remote = async {
        // read from local, write to remote
        match io::copy(&mut local_r, &mut remote_w).await {
            Ok(n) => {
                debug!("copied {} bytes from local to remote", n);
                let _ = remote_w.shutdown().await;
            }
            Err(err) => {
                error!(err = ?err, "failed to copy from local to remote")
            }
        }
    };

    tokio::join!(remote_to_me_to_local, local_to_me_to_remote);

    Ok(())
}
