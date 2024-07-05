use clap::Parser;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::prelude::*;
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
    let args = Args::parse();
    info!("server args: {:?}", args);

    // spawn the console server in the background
    let console_layer = console_subscriber::ConsoleLayer::builder()
        .with_default_env()
        .spawn();

    // build a `Subscriber` by combining layers with a
    // `tracing_subscriber::Registry`:
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(console_layer)
        .init();

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
