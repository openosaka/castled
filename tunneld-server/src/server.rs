use std::{net::ToSocketAddrs, pin::Pin};

use crate::Config;
use anyhow::Context;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use tonic::{transport::Server as GrpcServer, Request, Response, Status, Streaming};
use tunneld_protocol::pb::{
    tunnel_service_server::{TunnelService, TunnelServiceServer},
    ControlStream, RegisterReq, TrafficStream, UnRegisterReq, UnRegisteredResp,
};

#[derive(Debug, Default)]
pub struct Handler {
    _priv: (),
}

type GrpcResult<T> = Result<T, Status>;
type GrpcResponse<T> = GrpcResult<Response<T>>;
type RegisterStream = Pin<Box<dyn Stream<Item = GrpcResult<ControlStream>> + Send>>;
type DataStream = Pin<Box<dyn Stream<Item = GrpcResult<TrafficStream>> + Send>>;

pub struct Server {
    config: Config,
    server: GrpcServer,
}

impl Server {
    pub fn new(config: Config) -> Self {
        let server = GrpcServer::builder();

        Self { config, server }
    }

    pub async fn run(&mut self, cancel: CancellationToken) -> anyhow::Result<()> {
        let _ = cancel;
        let addr = format!("[::1]:{}", self.config.control_port)
            .to_socket_addrs()
            .context("parse control port")?
            .next()
            .context("invalid control_port")
            .unwrap();
        let handler = Handler::default();

        self.server
            .add_service(TunnelServiceServer::new(handler))
            .serve_with_shutdown(addr, cancel.cancelled())
            .await?;
        Ok(())
    }
}

#[tonic::async_trait]
impl TunnelService for Handler {
    type RegisterStream = RegisterStream;

    async fn register(&self, req: Request<RegisterReq>) -> GrpcResponse<self::RegisterStream> {
        todo!("register")
    }

    async fn un_register(&self, req: Request<UnRegisterReq>) -> GrpcResponse<UnRegisteredResp> {
        todo!()
    }

    type DataStream = DataStream;

    async fn data(&self, req: Request<Streaming<TrafficStream>>) -> GrpcResponse<self::DataStream> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server() {
        let mut server = Server::new(Config{
            control_port: 8610,
            http_port: 8611,
            domain: "".to_string(),
        });
        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();

        tokio::spawn(async move {
            let result = server.run(cancel).await;
            assert!(result.is_ok());
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
