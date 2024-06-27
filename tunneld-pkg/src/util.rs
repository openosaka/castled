use tokio::net::TcpListener;
use tonic::Status;
use tracing::error;

pub async fn create_listener(port: u16) -> Result<TcpListener, Status> {
    TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::AddrInUse => Status::already_exists("port already in use"),
            std::io::ErrorKind::PermissionDenied => Status::permission_denied("permission denied"),
            _ => {
                error!("failed to bind port: {}", err);
                Status::internal("failed to bind port")
            }
        })
}
