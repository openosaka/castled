use std::net::SocketAddr;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::future::join_all;
use tokio::{
    io::{self, AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::mpsc,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{transport::Channel, Streaming};
use tracing::{debug, error, instrument};
use tunneld_pkg::{
    io::{StreamingReader, StreamingWriter, TrafficToServerWrapper},
    select_with_shutdown, shutdown,
};
use tunneld_protocol::pb::{
    control::Payload,
    traffic_to_server,
    tunnel::{self, Type},
    tunnel_service_client::TunnelServiceClient,
    Command, Control, HttpConfig, RegisterReq, TcpConfig, TrafficToClient, TrafficToServer, Tunnel,
    UdpConfig,
};

pub struct Client<'a> {
    control_addr: &'a SocketAddr,
    tcp_tunnels: Vec<TcpTunnel>,
    udp_tunnels: Vec<UdpTunnel>,
    http_tunnels: Vec<HttpTunnel>,
}

struct TcpTunnel {
    name: String,
    remote_port: u16,
    local_endpoint: SocketAddr,
}

struct UdpTunnel {
    name: String,
    remote_port: u16,
    local_endpoint: SocketAddr,
}

struct HttpTunnel {
    name: String,
    remote_port: u16,
    local_endpoint: SocketAddr,
    subdomain: Bytes,
    domain: Bytes,
}

impl<'a> Client<'a> {
    pub fn new(addr: &'a SocketAddr) -> Result<Self> {
        Ok(Self {
            control_addr: addr,
            tcp_tunnels: Vec::new(),
            udp_tunnels: Vec::new(),
            http_tunnels: Vec::new(),
        })
    }

    pub async fn run(&mut self, shutdown_listener: shutdown::ShutdownListener) -> Result<()> {
        let mut tasks = Vec::new();
        for tcp_tunnel in self.tcp_tunnels.iter() {
            let tunnel = Tunnel {
                name: tcp_tunnel.name.to_string(),
                r#type: Type::Tcp as i32,
                config: Some(tunnel::Config::Tcp(TcpConfig {
                    remote_port: tcp_tunnel.remote_port as i32,
                })),
                ..Default::default()
            };
            tasks.push(self.handle_tunnel(
                shutdown_listener.clone(),
                tunnel,
                tcp_tunnel.local_endpoint,
            ));
        }

        for udp_tunnel in self.udp_tunnels.iter() {
            let tunnel = Tunnel {
                name: udp_tunnel.name.to_string(),
                r#type: Type::Udp as i32,
                config: Some(tunnel::Config::Udp(UdpConfig {
                    remote_port: udp_tunnel.remote_port as i32,
                })),
                ..Default::default()
            };
            tasks.push(self.handle_tunnel(
                shutdown_listener.clone(),
                tunnel,
                udp_tunnel.local_endpoint,
            ));
        }

        for http_tunnel in self.http_tunnels.iter() {
            let tunnel = Tunnel {
                name: http_tunnel.name.to_string(),
                r#type: Type::Http as i32,
                config: Some(tunnel::Config::Http(HttpConfig {
                    remote_port: http_tunnel.remote_port as i32,
                    subdomain: String::from_utf8_lossy(http_tunnel.subdomain.to_vec().as_slice())
                        .to_string(),
                    domain: String::from_utf8_lossy(http_tunnel.domain.to_vec().as_slice())
                        .to_string(),
                })),
                ..Default::default()
            };
            tasks.push(self.handle_tunnel(
                shutdown_listener.clone(),
                tunnel,
                http_tunnel.local_endpoint,
            ));
        }

        let results = join_all(tasks).await;
        // check if there is any error, if so, return the first error
        for result in results {
            result?
        }

        Ok(())
    }

    pub fn add_tcp_tunnel(&mut self, name: String, local_endpoint: SocketAddr, remote_port: u16) {
        debug!(
            "Registering TCP tunnel, remote_port: {}, local_endpoint: {}",
            remote_port, local_endpoint,
        );
        self.tcp_tunnels.push(TcpTunnel {
            name,
            remote_port,
            local_endpoint,
        });
    }

    pub fn add_udp_tunnel(&mut self, name: String, local_endpoint: SocketAddr, remote_port: u16) {
        debug!(
            "Registering UDP tunnel, remote_port: {}, local_endpoint: {}",
            remote_port, local_endpoint,
        );
        self.udp_tunnels.push(UdpTunnel {
            name,
            remote_port,
            local_endpoint,
        });
    }

    pub fn add_http_tunnel(
        &mut self,
        name: String,
        local_endpoint: SocketAddr,
        remote_port: u16,
        subdomain: Bytes,
        domain: Bytes,
    ) {
        self.http_tunnels.push(HttpTunnel {
            name,
            remote_port,
            local_endpoint,
            subdomain,
            domain,
        });
    }

    async fn handle_tunnel(
        &self,
        shutdown_listener: shutdown::ShutdownListener,
        tunnel: Tunnel,
        local_endpoint: SocketAddr,
    ) -> Result<()> {
        let is_udp = tunnel.r#type == Type::Udp as i32;
        let mut rpc_client = self.new_rpc_client().await?;
        let register = rpc_client.register(RegisterReq {
            tunnel: Some(tunnel),
        });

        select_with_shutdown!(register, shutdown_listener.done(), register_resp, {
            match register_resp {
                Err(e) => {
                    error!("failed to register tunnel: {:?}", e);
                    Err(e.into())
                }
                Ok(register_resp) => {
                    self.handle_control_stream(
                        shutdown_listener,
                        rpc_client,
                        register_resp,
                        local_endpoint,
                        is_udp,
                    )
                    .await
                }
            }
        })
    }

    async fn handle_control_stream(
        &self,
        shutdown_listener: shutdown::ShutdownListener,
        rpc_client: TunnelServiceClient<Channel>,
        register_resp: tonic::Response<Streaming<Control>>,
        local_endpoint: SocketAddr,
        is_udp: bool,
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
                                    debug!("received init command, tunnel initialized with id {}", init.tunnel_id);
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
                                        error!("failed to handle work traffic: {:?}", e);
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
                            error!("unexpected command: {:?}", control.command);
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

    async fn new_rpc_client(&self) -> Result<TunnelServiceClient<Channel>> {
        debug!("connecting server at {}", self.control_addr);

        TunnelServiceClient::connect(format!("http://{}", self.control_addr))
            .await
            .context("Failed to connect to the server")
            .map_err(Into::into)
    }
}

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

/// Forward the traffic from the server to the local.
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
                error!("failed to copy from remote to local: {:?}", err)
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
                error!("failed to copy from local to remote: {:?}", err)
            }
        }
    };

    tokio::join!(remote_to_me_to_local, local_to_me_to_remote);

    Ok(())
}
