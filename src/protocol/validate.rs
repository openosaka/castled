use super::pb::{
    tunnel::Config::Http, tunnel::Config::Tcp, tunnel::Config::Udp, tunnel::Type, RegisterReq,
};
use tonic::Status;

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
