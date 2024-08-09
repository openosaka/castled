use anyhow::{Context as _, Result};
use async_shutdown::{ShutdownManager, ShutdownSignal};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{transport::Channel, Response, Status, Streaming};
use tracing::{debug, error, info, instrument, span};

use tokio::{
    io::{self, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    select,
    sync::{mpsc, oneshot},
    time::timeout,
};

use crate::socket::Dialer;
use crate::{
    constant,
    io::{StreamingReader, StreamingWriter, TrafficToServerWrapper},
    pb::{
        self, control_command::Payload, traffic_to_server,
        tunnel_service_client::TunnelServiceClient, ControlCommand, RegisterReq, TrafficToClient,
        TrafficToServer,
    },
};

use super::tunnel::Tunnel;

/// Client represents a castle client that can register tunnels with the server.
#[derive(Clone)]
pub struct Client {
    grpc_client: TunnelServiceClient<Channel>,
}

impl Client {
    /// Creates a new `Client` instance with the specified control address.
    ///
    /// ```
    /// async fn run() {
    ///     let client = castled::client::Client::new("127.0.0.1:6100".parse().unwrap()).await.unwrap();
    /// }
    /// ```
    pub async fn new(addr: SocketAddr) -> Result<Self> {
        let grpc_client = new_rpc_client(addr).await?;
        Ok(Self { grpc_client })
    }

    /// Registers a tunnel with the server and returns a future that represents the tunnel handler.
    /// Also returns a receiver for receiving the assigned entrypoint from the server.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::net::SocketAddr;
    /// use castled::client::{
    ///     Client,
    ///     tunnel::Tunnel,
    ///     tunnel::RemoteConfig,
    ///     tunnel::HttpRemoteConfig,
    /// };
    /// use async_shutdown::ShutdownManager;
    ///
    /// async fn run1() {
    ///     let client = Client::new("127.0.0.1:6100".parse().unwrap()).await.unwrap();
    ///     let tunnel = Tunnel::new("my-tunnel", SocketAddr::from(([127, 0, 0, 1], 8971)), RemoteConfig::Tcp(8080));
    ///     let shutdown = ShutdownManager::new();
    ///     let entrypoint = client.start_tunnel(tunnel, shutdown.clone()).await.unwrap();
    ///     println!("entrypoint: {:?}", entrypoint);
    ///     shutdown.wait_shutdown_complete().await;
    /// }
    ///
    /// async fn run2() {
    ///     let client = Client::new("127.0.0.1:6100".parse().unwrap()).await.unwrap();
    ///     let tunnel = Tunnel::new("my-tunnel", SocketAddr::from(([127, 0, 0, 1], 8971)), RemoteConfig::Http(HttpRemoteConfig::RandomSubdomain));
    ///     let shutdown = ShutdownManager::new();
    ///     let entrypoint = client.start_tunnel(tunnel, shutdown.clone()).await.unwrap();
    ///     println!("entrypoint: {:?}", entrypoint);
    ///     shutdown.wait_shutdown_complete().await;
    /// }
    /// ```
    pub async fn start_tunnel(
        self,
        tunnel: Tunnel<'_>,
        shutdown: ShutdownManager<i8>,
    ) -> Result<Vec<String>> {
        let (entrypoint_tx, entrypoint_rx) = oneshot::channel();
        let pb_tunnel = tunnel.config.to_pb_tunnel(tunnel.name);
        let dialer = tunnel.dialer;

        tokio::spawn(async move {
            let _delay = shutdown.delay_shutdown_token();
            if let Err(err) = self
                .handle_tunnel(
                    shutdown.wait_shutdown_triggered(),
                    pb_tunnel,
                    dialer,
                    Some(move |entrypoint| {
                        let _ = entrypoint_tx.send(entrypoint);
                    }),
                )
                .await
            {
                error!(?err, "failed to handle tunnel");
                shutdown.trigger_shutdown_token(1)
            } else {
                shutdown.trigger_shutdown_token(0)
            }
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
        control_stream: &mut Streaming<ControlCommand>,
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
                let command = result.unwrap();
                match command.payload {
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
            }
        }
        Err(anyhow::anyhow!("failed to register tunnel"))
    }

    async fn register_tunnel(
        &self,
        rpc_client: &mut TunnelServiceClient<Channel>,
        tunnel: pb::Tunnel,
    ) -> Result<Response<Streaming<ControlCommand>>> {
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
        tunnel: pb::Tunnel,
        dial: Dialer,
        hook: Option<impl FnOnce(Vec<String>) + Send + 'static>,
    ) -> Result<()> {
        let mut rpc_client = self.grpc_client.clone();
        let response = timeout(
            Duration::from_secs(3),
            self.register_tunnel(&mut rpc_client, tunnel),
        )
        .await??;

        self.handle_control_stream(
            shutdown.clone(),
            rpc_client,
            response.into_inner(),
            dial,
            hook,
        )
        .await
    }

    /// Handles the control stream from the server.
    #[instrument(skip(self, shutdown, rpc_client, control_stream, hook))]
    async fn handle_control_stream(
        &self,
        shutdown: ShutdownSignal<i8>,
        rpc_client: TunnelServiceClient<Channel>,
        mut control_stream: Streaming<ControlCommand>,
        dialer: Dialer,
        mut hook: Option<impl FnOnce(Vec<String>) + Send + 'static>,
    ) -> Result<()> {
        let entrypoint = self
            .wait_until_registered(shutdown.clone(), &mut control_stream)
            .await?;
        if let Some(hook) = hook.take() {
            hook(entrypoint);
        }

        let dialer = Arc::new(dialer);
        loop {
            tokio::select! {
                result = control_stream.next() => {
                    if result.is_none() {
                        debug!("control stream closed");
                        break;
                    }
                    let command = result.unwrap()?;
                    match command.payload {
                        Some(Payload::Init(_)) => {
                            error!("unexpected init command");
                        }
                        Some(Payload::Work(work)) => {
                            debug!("received work command, starting to forward traffic");
                            let rpc_client = rpc_client.clone();
                            let dialer = dialer.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_work_traffic(
                                    rpc_client,
                                    &work.connection_id,
                                    dialer,
                                ).await {
                                    error!(?err, "failed to handle work traffic");
                                }
                            });
                        }
                        None => {
                            error!("missing payload in work command");
                            return Err(anyhow::anyhow!("missing payload in work command"));
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
}

#[instrument]
async fn new_rpc_client(control_addr: SocketAddr) -> Result<TunnelServiceClient<Channel>> {
    debug!("connecting server");

    TunnelServiceClient::connect(format!("http://{}", control_addr))
        .await
        .context("Failed to connect to the server")
        .map_err(Into::into)
}

/// Handles the work traffic from the server to the local endpoint.
#[instrument(skip(rpc_client))]
async fn handle_work_traffic(
    mut rpc_client: TunnelServiceClient<Channel>,
    connection_id: &str,
    dialer: Arc<Dialer>,
) -> Result<()> {
    // write response to the streaming_tx
    // rpc_client sends the data from reading the streaming_rx
    let (streaming_tx, streaming_rx) = mpsc::channel::<TrafficToServer>(64);
    let streaming_to_server = ReceiverStream::new(streaming_rx);

    let mut streaming_response = rpc_client
        .data(streaming_to_server)
        .await
        .unwrap()
        .into_inner();

    let connection_id = connection_id.to_string();
    let streaming_tx_for_first_msg = streaming_tx.clone();
    let start_message = TrafficToServer {
        connection_id: connection_id.clone(),
        action: traffic_to_server::Action::Start as i32,
        ..Default::default()
    };

    // write the data streaming response to transfer_tx,
    // then forward_traffic_to_local can read the data from transfer_rx
    let (transfer_tx, transfer_rx) = mpsc::channel::<TrafficToClient>(64);

    let (local_conn_established_tx, local_conn_established_rx) = mpsc::channel::<()>(1);
    let mut local_conn_established_rx = Some(local_conn_established_rx);
    tokio::spawn(async move {
        if local_conn_established_rx
            .take()
            .unwrap()
            .recv()
            .await
            .is_none()
        {
            return;
        } else {
            info!("local connection established");
        }

        // the first message to notify the server this connection is started to send data
        streaming_tx_for_first_msg
            .send(start_message)
            .await
            .unwrap();

        loop {
            let result = streaming_response.next().await;

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
    let writer = StreamingWriter::new(streaming_tx.clone(), wrapper);

    tokio::spawn(async move {
        match dialer.dial().await {
            Ok((local_r, local_w)) => {
                local_conn_established_tx.send(()).await.unwrap();
                if let Err(err) =
                    transfer(local_r, local_w, StreamingReader::new(transfer_rx), writer).await
                {
                    debug!("failed to forward traffic to local: {:?}", err);
                }
            }
            Err(err) => {
                error!(
                    local_endpoint = ?dialer.addr(),
                    ?err,
                    "failed to connect to local endpoint, so let's notify the server to close the user connection",
                );

                streaming_tx
                    .send(TrafficToServer {
                        connection_id: connection_id.to_string(),
                        action: traffic_to_server::Action::Close as i32,
                        ..Default::default()
                    })
                    .await
                    .context("terrible, the server may be crashed")
                    .unwrap();
            }
        }
    });

    Ok(())
}

/// transfer the traffic from the server to the local endpoint.
///
/// Try to imagine the current client is yourself,
/// your mission is to forward the traffic from the server to the local,
/// then write the original response back to the server.
/// in this process, there are two underlying connections:
/// 1. remote <=> me
/// 2. me     <=> local
async fn transfer(
    local_r: impl AsyncRead + Unpin,
    mut local_w: impl AsyncWrite + Unpin,
    remote_r: impl AsyncRead + Unpin,
    mut remote_w: impl AsyncWrite + Unpin,
) -> Result<()> {
    let remote_to_me_to_local = async {
        // read from remote, write to local
        match io::copy_buf(
            &mut BufReader::with_capacity(constant::DEFAULT_BUF_SIZE, remote_r),
            &mut local_w,
        )
        .await
        {
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
        match io::copy_buf(
            &mut BufReader::with_capacity(constant::DEFAULT_BUF_SIZE, local_r),
            &mut remote_w,
        )
        .await
        {
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
