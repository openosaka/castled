use std::net::SocketAddr;

use anyhow::{Context, Result};
use futures::future::join_all;
use tokio::net::TcpStream;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error};
use tunneld_protocol::pb::{
    tunnel::{self, Type},
    tunnel_service_client::TunnelServiceClient,
    RegisterReq, TcpConfig, Tunnel,
};

pub struct Client<'a> {
    control_addr: &'a SocketAddr,
    tunnels: Vec<TcpTunnel>,
}

struct TcpTunnel {
    name: String,
    remote_port: u16,
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
        for tunnel in &self.tunnels {
            tasks.push(self.register_tcp(cancel.clone(), tunnel));
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

    pub fn add_tcp_tunnel(&mut self, name: String, remote_port: u16) {
        debug!("Registering TCP tunnel, remote_port: {}", remote_port);
        let addr = self.control_addr.clone();
        self.tunnels.push(TcpTunnel { name, remote_port });
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
        loop {
            tokio::select! {
                result = control_stream.next() => {
                    match result {
                        Some(result) => {
                            match result {
                                Ok(control) => {
                                    debug!("received control message: {:?}", control);
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

    async fn dial(&self) -> Result<TcpStream> {
        TcpStream::connect(self.control_addr)
            .await
            .map_err(Into::into)
    }
}
