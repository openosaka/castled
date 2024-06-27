use tonic::Status;
use crate::pb::{
	tunnel::Type, RegisterReq,
	tunnel::Config::Http,
	tunnel::Config::Tcp,
};

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
		Type::Tcp => {
			match config {
				Http(_) => {
					return Some(Status::invalid_argument("tcp is required"));
				}
				Tcp(tcp) => {
					if tcp.remote_port == 0 {
						return Some(Status::invalid_argument("remote_port is required"));
					}
				}
			}
		}
		Type::Http => {
			match config {
				Tcp(_) => {
					return Some(Status::invalid_argument("http is required"));
				}
				Http(h) => {
					if h.remote_port == 0 || h.subdomain.is_empty() || h.domain.is_empty() {
						return Some(Status::invalid_argument("at least remote_port, subdomain and domain is required"));
					}
				}
			}
		}
	}
    None
}
