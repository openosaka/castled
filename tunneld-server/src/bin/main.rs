use clap::Parser;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tunneld_pkg::otel::setup_logging;
use tunneld_server::Server;

#[derive(Parser, Debug, Default)]
struct Args {
    #[arg(long, default_value = "6610")]
    control_port: u16,

    #[arg(long, default_value = "6611")]
    vhttp_port: u16,

    /// Domain name for the http server, it could be empty,
    /// the client can't register with domain if it's empty.
    ///
    /// e.g. "tunnel.example.com", don't include the protocol.
    #[arg(long, default_value = "", required = false)]
    domain: String,
}

#[tokio::main]
async fn main() {
    setup_logging();

    let args = Args::parse();
    info!("server args: {:?}", args);

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

    let server = Server::new(args.control_port, args.vhttp_port, args.domain);
    if let Err(err) = server.run(cancel.cancelled()).await {
        eprintln!("server error: {:?}", err);
    }
}
