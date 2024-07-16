use anyhow::{Context, Result};
use futures::Future;
use std::{net::SocketAddr, pin::Pin};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{transport::Channel, Streaming};
use tracing::{debug, error, info, instrument, span};

use tokio::{
    io::{self, AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::{mpsc, oneshot},
};

use crate::{
    io::{StreamingReader, StreamingWriter, TrafficToServerWrapper},
    protocol::pb::{
        self, control::Payload, traffic_to_server, tunnel::Type,
        tunnel_service_client::TunnelServiceClient, Command, Control, RegisterReq, TrafficToClient,
        TrafficToServer,
    },
    select_with_shutdown, shutdown,
};

use super::tunnel::Tunnel;

/// Client represents a castle client that can register tunnels with the server.
pub struct Client {
    control_addr: SocketAddr,
}

/// TunnelFuture is a future that represents the tunnel handler.
///
/// The tunnel future is responsible for handling the tunnel process.
pub type TunnelFuture<'a> = Pin<Box<dyn Future<Output = Result<(), anyhow::Error>> + Send + 'a>>;

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
    /// use castled::shutdown::Shutdown;
    /// use castled::client::{
    ///     Client,
    ///     tunnel::new_tcp_tunnel,
    /// };
    ///
    /// let client = Client::new("127.0.0.1:6100".parse().unwrap());
    /// let tunnel = new_tcp_tunnel(String::from("my-tunnel"), SocketAddr::from(([127, 0, 0, 1], 8971)), 8080);
    /// let shutdown_listener = Shutdown::new().listen();
    /// let (handler, entrypoint_rx) = client.register_tunnel(tunnel, shutdown_listener).unwrap();
    /// tokio::spawn(async move {
    ///     let entrypoint = entrypoint_rx.await.unwrap();
    /// });
    /// // handler.await;
    /// ```
    pub fn register_tunnel(
        &self,
        tunnel: Tunnel,
        shutdown_listener: shutdown::ShutdownListener,
    ) -> Result<(TunnelFuture, oneshot::Receiver<Vec<String>>)> {
        let (entrypoint_tx, entrypoint_rx) = oneshot::channel();
        let handler = self.handle_tunnel(
            shutdown_listener,
            tunnel.inner,
            tunnel.local_endpoint,
            Some(|entrypoint: Vec<String>| {
                if let Err(err) = entrypoint_tx.send(entrypoint) {
                    error!(err = ?err, "failed to send entrypoint the receiver may be dropped, check your code");
                }
            }),
        );
        Ok((Box::pin(handler), entrypoint_rx))
    }

    /// Handles the tunnel registration process.
    async fn handle_tunnel(
        &self,
        shutdown_listener: shutdown::ShutdownListener,
        tunnel: pb::Tunnel,
        local_endpoint: SocketAddr,
        hook: Option<impl FnOnce(Vec<String>) + Send + 'static>,
    ) -> Result<()> {
        let span = span!(
            tracing::Level::INFO,
            "register_tunnel",
            tunnel_name = tunnel.name
        );
        let _enter = span.enter();

        let is_udp = tunnel.r#type == Type::Udp as i32;
        let mut rpc_client = self.new_rpc_client().await?;
        let register = rpc_client.register(RegisterReq {
            tunnel: Some(tunnel),
        });

        select_with_shutdown!(register, shutdown_listener.done(), register_resp, {
            match register_resp {
                Err(e) => {
                    error!(err = ?e, "failed to register tunnel");
                    Err(e.into())
                }
                Ok(register_resp) => {
                    self.handle_control_stream(
                        shutdown_listener,
                        rpc_client,
                        register_resp,
                        local_endpoint,
                        is_udp,
                        hook,
                    )
                    .await
                }
            }
        })
    }

    /// Handles the control stream from the server.
    #[instrument(skip(self, shutdown_listener, rpc_client, register_resp, hook))]
    async fn handle_control_stream(
        &self,
        shutdown_listener: shutdown::ShutdownListener,
        rpc_client: TunnelServiceClient<Channel>,
        register_resp: tonic::Response<Streaming<Control>>,
        local_endpoint: SocketAddr,
        is_udp: bool,
        mut hook: Option<impl FnOnce(Vec<String>)>,
    ) -> Result<()> {
        let mut control_stream = register_resp.into_inner();
        let mut initialized = false;
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
                            if initialized {
                                error!("received duplicate init command");
                                break;
                            }
                            match control.payload {
                                Some(Payload::Init(init)) => {
                                    initialized = true;
                                    info!(
                                        tunnel_id = init.tunnel_id,
                                        entrypoint = ?init.assigned_entrypoint,
                                        "tunnel registered successfully",
                                    );
                                    if let Some(hook) = hook.take() {
                                        hook(init.assigned_entrypoint.clone());
                                    }
                                    continue; // the only path to success.
                                }
                                Some(Payload::Work(_)) => {
                                    error!("unexpected work command");
                                }
                                None => {
                                    error!("missing payload in init command");
                                }
                            }
                            break;
                        },
                        Ok(Command::Work) => {
                            if !initialized {
                                error!("received work command before init command");
                                break;
                            }
                            match control.payload {
                                Some(Payload::Init(_)) => {
                                    error!("unexpected init command");
                                }
                                Some(Payload::Work(work)) => {
                                    debug!("received work command, starting to forward traffic");
                                    if let Err(e) = handle_work_traffic(
                                        rpc_client.clone() /* cheap clone operation */,
                                        &work.connection_id,
                                        local_endpoint,
                                        is_udp,
                                    ).await {
                                        error!(err = ?e, "failed to handle work traffic");
                                    } else {
                                        continue; // the only path to success.
                                    }
                                }
                                None => {
                                    error!("missing payload in work command");
                                }
                            }
                            break;
                        },
                        _ => {
                            error!(command = %control.command, "unexpected command");
                        }
                    }
                }
                _ = shutdown_listener.done() => {
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

    tokio::spawn(async move {
        let mut streaming_response = rpc_client
            .data(streaming_to_server)
            .await
            .unwrap()
            .into_inner();

        loop {
            let result = streaming_response.next().await;
            match result {
                Some(Ok(traffic)) => {
                    transfer_tx.send(traffic).await.unwrap();
                    continue;
                }
                Some(Err(status)) => {
                    error!("received error status: {:?}", status);
                }
                None => {
                    // when the server finished traffic, it will close the data streaming
                    debug!("data streaming closed by the server");
                }
            }
            return;
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
