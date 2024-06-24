use std::{future, net::SocketAddr, pin::Pin, sync::Arc};

use anyhow::Result;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error};
use tracing_subscriber::field::debug;
use tunneld_protocol::pb::{
    tunnel::{self, Type},
    tunnel_service_client::TunnelServiceClient,
    RegisterReq, TcpConfig, Tunnel,
};

pub struct Client<'a> {
    rpc_client: tunneld_protocol::pb::tunnel_service_client::TunnelServiceClient<Channel>,

    server_addr: &'a SocketAddr,

    tunnels: Vec<TcpTunnel>,
}

struct TcpTunnel {
    remote_port: u16,
}

impl<'a> Client<'a> {
    pub async fn new(addr: &'a SocketAddr) -> Result<Self> {
        // TODO(sword): tls,
        let client = TunnelServiceClient::connect(format!("http://{}", addr)).await?;

        Ok(Self {
            rpc_client: client,
            server_addr: addr,
            tunnels: Vec::new(),
        })
    }

    // TODO(sword): only support one tunnel now.
    pub async fn run(&mut self, cancel: CancellationToken) -> Result<()> {
        let tunnel = self.tunnels.first().unwrap();
        if let Err(e) = self.register_tcp(tunnel.remote_port).await {
            error!("Failed to handle tunnel: {:?}", e);
        }

        cancel.cancelled().await;
        Ok(())
    }

    pub fn add_tcp_tunnel(&mut self, remote_port: u16) {
        debug!("Registering TCP tunnel to port {}", remote_port);
        let addr = self.server_addr.clone();
        self.tunnels.push(TcpTunnel { remote_port });
    }

    async fn register_tcp(&mut self, remote_port: u16) -> Result<()> {
        let resgiter_resp = self
            .rpc_client
            .register(RegisterReq {
                client_version: "todo".to_string(),
                tunnel: Some(Tunnel {
                    name: "todo".to_string(),
                    r#type: Type::Tcp as i32,
                    config: Some(tunnel::Config::Tcp(TcpConfig {
                        remote_port: remote_port as i32,
                    })),
                    ..Default::default()
                }),
            })
            .await?;
        let control_stream = resgiter_resp.into_inner();
        // todo(sword): listen on control stream

        Ok(())
    }

    pub fn add_http_tunnel(&self, remote_port: u16, subdomain: &str, domain: &str) {
        println!(
            "Registering HTTP tunnel to port {}, subdomain: {}, domain: {}",
            remote_port, subdomain, domain
        );
        panic!("Not implemented");
    }

    async fn dial(&self) -> Result<TcpStream> {
        TcpStream::connect(self.server_addr)
            .await
            .map_err(Into::into)
    }
}
