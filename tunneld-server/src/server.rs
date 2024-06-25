use std::{net::ToSocketAddrs, pin::Pin, thread::spawn};

use crate::validate::validate_register_req;
use crate::Config;
use anyhow::Context as _;
use core::task::{Context, Poll};
use tokio::{
    net::TcpListener,
    select,
    sync::{
        mpsc::{self, unbounded_channel, UnboundedSender},
        oneshot,
    },
    time::sleep,
};
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tonic::{transport::Server as GrpcServer, Request, Response, Status, Streaming};
use tracing::{debug, error};
use tunneld_protocol::pb::{
    control::Payload,
    tunnel::Config::Tcp,
    tunnel_service_server::{TunnelService, TunnelServiceServer},
    Command, Control, InitPayload, RegisterReq, Traffic, UnRegisterReq, UnRegisteredResp,
    WorkPayload,
};

#[derive(Debug, Default)]
pub struct Handler {
    _priv: (),
}

type GrpcResult<T> = Result<T, Status>;
type GrpcResponse<T> = GrpcResult<Response<T>>;
type RegisterStream = Pin<Box<CancelableReceiver<GrpcResult<Control>>>>;
type DataStream = Pin<Box<dyn Stream<Item = GrpcResult<Traffic>> + Send>>;

pub struct CancelableReceiver<T> {
    cancel: CancellationToken,
    inner: mpsc::Receiver<T>,
}

impl<T> CancelableReceiver<T> {
    pub fn new(cancel: CancellationToken, inner: mpsc::Receiver<T>) -> Self {
        Self { cancel, inner }
    }
}

impl<T> Stream for CancelableReceiver<T> {
    type Item = T;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.inner.poll_recv(cx)
    }
}

impl<T> std::ops::Deref for CancelableReceiver<T> {
    type Target = mpsc::Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> Drop for CancelableReceiver<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub struct Server {
    config: Config,
    server: GrpcServer,
    close: CancellationToken,
}

impl Server {
    /// Create a new server instance.
    pub fn new(config: Config) -> Self {
        let server = GrpcServer::builder();
        let close = CancellationToken::new();

        Self {
            config,
            server,
            close,
        }
    }

    pub async fn run(&mut self, cancel: CancellationToken) -> anyhow::Result<()> {
        let addr = format!("0.0.0.0:{}", self.config.control_port)
            .to_socket_addrs()
            .context("parse control port")?
            .next()
            .context("invalid control_port")
            .unwrap();
        let handler = Handler::default();

        let parent_cancel = cancel.clone();
        let server_cancel = self.close.clone();
        tokio::spawn(async move {
            parent_cancel.cancelled().await;
            server_cancel.cancel(); // cancel myself.
        });

        debug!("starting server on: {}", addr);

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
        let req = req.into_inner();
        if let Some(status) = validate_register_req(&req) {
            return Err(status);
        }

        let (tx, rx) = mpsc::channel(1024);
        let init_command = Control {
            command: Command::Init as i32,
            payload: Some(Payload::Init(InitPayload {
                server_version: "todo".to_string(),
                tunnel_id: "todo".to_string(),
                assigned_entrypoint: "todo".to_string(),
            })),
        };
        tx.send(Ok(init_command)).await.map_err(|err| {
            error!("failed to send control stream: {}", err);
            Status::internal("failed to send control stream")
        })?;

        let cancel_listener_w = CancellationToken::new();
        let cancel_listener = cancel_listener_w.clone();

        // TODO(sword): support more tunnel types
        // let's assume it's a TCP tunnel for now
        if let Tcp(tcp) = req.tunnel.as_ref().unwrap().config.as_ref().unwrap() {
            let remote_port = tcp.remote_port.to_owned();
            // create a new tcp listener on the remote port
            let listener = self.create_listener(remote_port as u16).await?;

            tokio::spawn(async move {
                loop {
                    select! {
                        _ = cancel_listener.cancelled() => {
                            return;
                        }
                        result = listener.accept() => {
                            let (mut stream, addr) = result.unwrap();
                            debug!("new user connection from: {:?}", addr);

                            tx.send(Ok(Control {
                                command: Command::Work as i32,
                                payload: Some(Payload::Work(WorkPayload {})),
                            }))
                            .await
                            .context("failed to notify client to receive traffic")
                            .unwrap();

                            tokio::spawn(async move {
                                todo!("handle data stream");
                            });
                        }
                    }
                }
            });
        } else {
            unimplemented!("only support TCP tunnel for now")
        }

        let control_stream =
            Box::pin(CancelableReceiver::new(cancel_listener_w, rx)) as self::RegisterStream;
        Ok(Response::new(control_stream))
    }

    async fn un_register(&self, req: Request<UnRegisterReq>) -> GrpcResponse<UnRegisteredResp> {
        todo!()
    }

    type DataStream = DataStream;

    async fn data(&self, req: Request<Streaming<Traffic>>) -> GrpcResponse<self::DataStream> {
        todo!()
    }
}

impl Handler {
    async fn create_listener(&self, port: u16) -> Result<TcpListener, Status> {
        TcpListener::bind(("0.0.0.0", port))
            .await
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::AddrInUse => Status::already_exists("port already in use"),
                std::io::ErrorKind::PermissionDenied => {
                    Status::permission_denied("permission denied")
                }
                _ => {
                    error!("failed to bind port: {}", err);
                    Status::internal("failed to bind port")
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server() {
        let mut server = Server::new(Config {
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

        sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
