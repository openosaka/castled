use crate::pb::{
    tunnel::Config::Http, tunnel::Config::Tcp, tunnel::Config::Udp, tunnel::Type, RegisterReq,
};

use tokio::net::{TcpListener, UdpSocket};
use tonic::Status;
use tracing::error;

pub(crate) async fn create_tcp_listener(port: u16) -> Result<TcpListener, Status> {
    TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(map_bind_error)
}

pub(crate) async fn create_udp_socket(port: u16) -> Result<UdpSocket, Status> {
    UdpSocket::bind(("0.0.0.0", port))
        .await
        .map_err(map_bind_error)
}

fn map_bind_error(err: std::io::Error) -> Status {
    match err.kind() {
        std::io::ErrorKind::AddrInUse => Status::already_exists("port already in use"),
        std::io::ErrorKind::PermissionDenied => Status::permission_denied("permission denied"),
        _ => {
            error!("failed to bind port: {}", err);
            Status::internal("failed to bind port")
        }
    }
}

pub fn validate_register_req(req: &RegisterReq) -> Option<Status> {
    if req.tunnel.is_none() {
        return Some(Status::invalid_argument("tunnel is required"));
    }
    let tunnel = req.tunnel.as_ref().unwrap();
    if tunnel.config.is_none() {
        return Some(Status::invalid_argument("config is required"));
    }
    let config = tunnel.config.as_ref().unwrap();
    match tunnel.r#type() {
        Type::Tcp => match config {
            Tcp(_) => {}
            _ => {
                return Some(Status::invalid_argument("tcp is required"));
            }
        },
        Type::Http => match config {
            Http(_) => {}
            _ => {
                return Some(Status::invalid_argument("http is required"));
            }
        },
        Type::Udp => match config {
            Udp(_) => {}
            _ => {
                return Some(Status::invalid_argument("udp is required"));
            }
        },
    }
    None
}
