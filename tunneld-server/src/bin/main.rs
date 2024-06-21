use clap::Parser;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tunneld_server::{Config, Server};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::parse();
    info!("server config: {:?}", config);

    let cancel_w = CancellationToken::new();
    let cancel = cancel_w.clone();

    tokio::spawn(async move {
        if let Err(e) = signal::ctrl_c().await {
            // Something really weird happened. So just panic
            panic!("Failed to listen for the ctrl-c signal: {:?}", e);
        }
		info!("Received ctrl-c signal. Shutting down...");
        cancel_w.cancel();
    });

    let mut server = Server::new(config);
    if let Err(err) = server.run(cancel).await {
        eprintln!("server error: {:?}", err);
    }
}
