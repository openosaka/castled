use std::net::SocketAddr;

use anyhow::{Context, Result};
use futures::future::join_all;
use tokio::{
    io::{self, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error};
use tunneld_pkg::io::{StreamingReader, StreamingWriter, TrafficToServerWrapper, WriteDataWrapper};
use tunneld_protocol::pb::{
    control::Payload,
    traffic_to_server,
    tunnel::{self, Type},
    tunnel_service_client::TunnelServiceClient,
    Command, RegisterReq, TcpConfig, TrafficToClient, TrafficToServer, Tunnel,
};

pub struct Client<'a> {
    control_addr: &'a SocketAddr,
    tunnels: Vec<TcpTunnel>,
}

struct TcpTunnel {
    name: String,
    remote_port: u16,
    local_port: u16,
}

impl<'a> Client<'a> {
    pub fn new(addr: &'a SocketAddr) -> Result<Self> {
        Ok(Self {
            control_addr: addr,
            tunnels: Vec::new(),
        })
    }

    pub async fn run(&mut self, cancel: CancellationToken) -> Result<()> {
        let mut tasks = Vec::new();
        for tunnel in self.tunnels.iter() {
            tasks.push(self.register_tcp(cancel.clone(), &tunnel));
        }
        let results = join_all(tasks).await;

        // log the errors
        results.iter().enumerate().for_each(|(i, r)| {
            if let Err(e) = r {
                error!("tunnel {}: {:?}", self.tunnels[i].name, e);
            }
        });

        Ok(())
    }

    pub fn add_tcp_tunnel(&mut self, name: String, remote_port: u16, local_port: u16) {
        debug!(
            "Registering TCP tunnel, remote_port: {}, local_port: {}",
            remote_port, local_port
        );
        self.tunnels.push(TcpTunnel {
            name,
            remote_port,
            local_port,
        });
    }

    async fn register_tcp(&self, cancel: CancellationToken, tcp: &TcpTunnel) -> Result<()> {
        let mut rpc_client = self.new_rpc_client().await?;
        let register_resp = rpc_client
            .register(RegisterReq {
                client_version: "todo".to_string(),
                tunnel: Some(Tunnel {
                    name: tcp.name.to_string(),
                    r#type: Type::Tcp as i32,
                    config: Some(tunnel::Config::Tcp(TcpConfig {
                        remote_port: tcp.remote_port as i32,
                    })),
                    ..Default::default()
                }),
            })
            .await?;
        let mut control_stream = register_resp.into_inner();
        let mut initialized = false;
        loop {
            tokio::select! {
                result = control_stream.next() => {
                    match result {
                        Some(result) => {
                            match result {
                                Ok(control) => {
                                    match Command::try_from(control.command) {
                                        Ok(Command::Init) => {
                                            if initialized {
                                                error!("received duplicate init command");
                                                continue;
                                            }
                                            match control.payload {
                                                Some(Payload::Init(init)) => {
                                                    initialized = true;
                                                    debug!("received init command, tunnel initialized with id {}", init.tunnel_id);
                                                }
                                                Some(Payload::Work(_)) => {
                                                    error!("unexpected work command");
                                                    continue;
                                                }
                                                None => {
                                                    error!("missing payload in init command");
                                                    continue;
                                                }
                                            }
                                        },
                                        Ok(Command::Work) => {
                                            if !initialized {
                                                error!("received work command before init command");
                                                continue;
                                            }
                                            match control.payload {
                                                Some(Payload::Init(_)) => {
                                                    error!("unexpected init command");
                                                    continue;
                                                }
                                                Some(Payload::Work(work)) => {
                                                    debug!("received work command, starting to forward traffic");
                                                    if let Err(e) = handle_work_traffic(rpc_client.clone() /* cheap clone operation */, &work.connection_id, tcp.local_port).await {
                                                        error!("failed to handle work traffic: {:?}", e);
                                                        continue;
                                                    }
                                                }
                                                None => {
                                                    error!("missing payload in work command");
                                                    continue;
                                                }
                                            }
                                        },
                                        _ => {
                                            error!("unexpected command: {:?}", control.command);
                                        }
                                    }
                                },
                                Err(e) => {
                                    error!("received error: {:?}", e);
                                },
                            }
                        },
                        None => {
                            debug!("control stream ended");
                            break;
                        },
                    }
                }
                _ = cancel.cancelled() => {
                    debug!("canceling tcp tunnel");
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    pub fn add_http_tunnel(&self, remote_port: u16, subdomain: &str, domain: &str) {
        println!(
            "registering HTTP tunnel to port {}, subdomain: {}, domain: {}",
            remote_port, subdomain, domain
        );
        panic!("Not implemented");
    }

    async fn new_rpc_client(&self) -> Result<TunnelServiceClient<Channel>> {
        debug!("connecting server at {}", self.control_addr);

        TunnelServiceClient::connect(format!("http://{}", self.control_addr))
            .await
            .context("Failed to connect to the server")
            .map_err(Into::into)
    }
}

async fn handle_work_traffic(
    mut rpc_client: TunnelServiceClient<Channel>,
    connection_id: &str,
    local_port: u16,
) -> Result<()> {
    // write response to the streaming_tx
    // rpc_client sends the data from reading the streaming_rx
    let (streaming_tx, streaming_rx) = mpsc::channel::<TrafficToServer>(1024);
    let streaming_to_server = ReceiverStream::new(streaming_rx);

    // the first message to notify the server this connection is started to send data
    streaming_tx
        .send(TrafficToServer {
            connection_id: connection_id.to_string(),
            action: traffic_to_server::Action::Start as i32,
            ..Default::default()
        })
        .await
        .unwrap();

    // write the data streaming response to transfer_tx,
    // then forward_traffic_to_local can read the data from transfer_rx
    let (transfer_tx, transfer_rx) = mpsc::channel::<TrafficToClient>(1024);

    tokio::spawn(async move {
        let mut streaming_response = rpc_client
            .data(streaming_to_server)
            .await
            .unwrap()
            .into_inner();
        while let Some(traffic) = streaming_response.next().await {
            transfer_tx.send(traffic.unwrap()).await.unwrap();
        }
    });

    let wrapper = TrafficToServerWrapper::new(connection_id.to_string());
    let writer = StreamingWriter::new(streaming_tx, wrapper);
    tokio::spawn(forward_traffic_to_local(
        local_port,
        writer,
        StreamingReader::new(transfer_rx),
    ));

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
    local_port: u16,
    mut remote_w: StreamingWriter<TrafficToServer>,
    mut remote_r: StreamingReader<TrafficToClient>,
) -> Result<()> {
    let mut local_conn = TcpStream::connect(format!("0.0.0.0:{}", local_port))
        .await
        .context("failed to connect to local")?;
    let (mut local_r, mut local_w) = local_conn.split();

    let remote_to_me_to_local = async {
        // read from remote, write to local
        if let Ok(n) = io::copy(&mut remote_r, &mut local_w).await {
            debug!("copied {} bytes from remote to local", n);
            let _ = local_w.shutdown().await;
        }
    };

    let local_to_me_to_remote = async {
        // read from local, write to remote
        if let Ok(n) = io::copy(&mut local_r, &mut remote_w).await {
            debug!("copied {} bytes from local to remote", n);
            let _ = remote_w.shutdown().await;
        }
    };

    tokio::join!(remote_to_me_to_local, local_to_me_to_remote);

    Ok(())
}
