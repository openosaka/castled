use tonic::Status;

use crate::pb::RegisterReq;

pub fn validate_register_req(req: &RegisterReq) -> Option<Status> {
    if req.tunnel.is_none() {
        return Some(Status::invalid_argument("tunnel is required"));
    }
    let tunnel = req.tunnel.as_ref().unwrap();
    if tunnel.config.is_none() {
        return Some(Status::invalid_argument("config is required"));
    }
    None
}
