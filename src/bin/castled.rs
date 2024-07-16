use std::net::IpAddr;

use castled::{
    debug::setup_logging,
    server::{Config, EntrypointConfig, Server},
};
use clap::Parser;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Parser, Debug, Default)]
struct Args {
    #[arg(long, default_value = "6610")]
    control_port: u16,

    /// the vhttp server port, it serves all the http requests through the vhttp port.
    #[arg(long, default_value = "6611")]
    vhttp_port: u16,

    /// Domain names for the http server, it could be empty,
    /// the client can't register with domain if it's empty.
    ///
    /// e.g. "tunnel.example.com", don't include the protocol.
    #[arg(long, required = false)]
    domain: Vec<String>,

    /// The IP addresses of the castle server.
    #[arg(long, required = false)]
    ip: Vec<IpAddr>,

    /// If the vhttp server is behind a http proxy like nginx, set this to true.
    #[arg(long, default_value = "false")]
    vhttp_behind_proxy_tls: bool,

    /// Minimum accepted port number.
    #[clap(long, default_value_t = 1024)]
    random_min_port: u16,

    /// Maximum accepted port number.
    #[clap(long, default_value_t = 65535)]
    random_max_port: u16,
}

#[tokio::main]
async fn main() {
    // 6669 is the default tokio console server port of the server,
    // use `TOKIO_CONSOLE_BIND=127.0.0.1:6670` to change it.
    setup_logging(6669);

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

    let server = Server::new(Config {
        control_port: args.control_port,
        vhttp_port: args.vhttp_port,
        entrypoint: EntrypointConfig {
            domain: args.domain,
            ip: args.ip,
            vhttp_behind_proxy_tls: args.vhttp_behind_proxy_tls,
            port_range: args.random_min_port..=args.random_max_port,
        },
    });
    if let Err(err) = server.run(cancel.cancelled()).await {
        eprintln!("server error: {:?}", err);
    }
}
