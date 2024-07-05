use bytes::Bytes;
use clap::{Parser, Subcommand};
use std::net::{SocketAddr, ToSocketAddrs};
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::prelude::*;
use tunneld_pkg::shutdown;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, default_value = "127.0.0.1:6610")]
    server_addr: SocketAddr,
}

#[derive(Subcommand)]
enum Commands {
    Tcp {
        #[clap(index = 1)]
        port: u16,
        #[arg(long, required = true)]
        remote_port: u16,
        #[arg(
            long,
            default_value = "127.0.0.1",
            help = "Local address to bind to, e.g localhost, example.com"
        )]
        local_addr: String,
    },
    Http {
        #[clap(index = 1)]
        port: u16,
        #[arg(long)]
        remote_port: Option<u16>,
        #[arg(long)]
        subdomain: Option<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(
            long,
            default_value = "127.0.0.1",
            help = "Local address to bind to, e.g localhost, example.com"
        )]
        local_addr: String,
    },
}

const TUNNEL_NAME: &str = "tunneld-client";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // spawn the console server in the background
    let console_layer = console_subscriber::ConsoleLayer::builder()
        .with_default_env()
        .spawn();

    // build a `Subscriber` by combining layers with a
    // `tracing_subscriber::Registry`:
    tracing_subscriber::registry()
        .with(console_layer)
        .with(tracing_subscriber::fmt::layer())
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

    let mut client = tunneld_client::Client::new(&args.server_addr).unwrap();

    match args.command {
        Commands::Tcp {
            port,
            remote_port,
            local_addr,
        } => {
            let local_endpoint = parse_socket_addr(&local_addr, port)?;
            client.add_tcp_tunnel(TUNNEL_NAME.to_string(), local_endpoint, remote_port);
        }
        Commands::Http {
            port,
            local_addr,
            remote_port,
            subdomain,
            domain,
        } => {
            let local_endpoint = parse_socket_addr(&local_addr, port)?;
            client.add_http_tunnel(
                TUNNEL_NAME.to_string(),
                local_endpoint,
                remote_port.unwrap_or(0),
                Bytes::from(subdomain.unwrap_or_default()),
                Bytes::from(domain.unwrap_or_default()),
            );
        }
    }

    client
        .run(shutdown::ShutdownListener::from_cancellation(cancel))
        .await
}

fn parse_socket_addr(local_addr: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let mut addrs = format!("{}:{}", local_addr, port).to_socket_addrs()?;
    if addrs.len() != 1 {
        return Err(anyhow::anyhow!("Invalid address"));
    }
    Ok(addrs.next().unwrap())
}
