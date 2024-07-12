use bytes::Bytes;
use clap::{Parser, Subcommand};
use std::net::{SocketAddr, ToSocketAddrs};
use tokio::{net::lookup_host, signal};
use tracing::info;
use tunneld_client::{
    tunnel::{new_http_tunnel, new_tcp_tunnel, new_udp_tunnel},
    TunnelFuture,
};
use tunneld_pkg::{otel::setup_logging, shutdown};

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
        /// remote port the client will forward the traffic to the local port.
        #[arg(long, required = false, default_value_t = 0)]
        remote_port: u16,
        #[arg(
            long,
            default_value = "127.0.0.1",
            help = "Local address to bind to, e.g localhost, 127.0.0.1"
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
            help = "Local address to bind to, e.g localhost, 127.0.0.1"
        )]
        local_addr: String,
        #[arg(long)]
        random_subdomain: bool,
    },
    Udp {
        #[clap(index = 1)]
        port: u16,
        #[arg(long, required = false, default_value_t = 0)]
        remote_port: u16,
        #[arg(
            long,
            default_value = "127.0.0.1",
            help = "Local address to bind to, e.g localhost, 127.0.0.1"
        )]
        local_addr: String,
    },
}

const TUNNEL_NAME: &str = "tunneld-client";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 6670 is the default tokio console server port of the client,
    // use `TOKIO_CONSOLE_BIND=127.0.0.1:6669` to change it.
    setup_logging(6670);

    let args = Args::parse();

    let shutdown = shutdown::Shutdown::new();

    let client = tunneld_client::Client::new(args.server_addr).unwrap();
    let entrypoint_rx: tokio::sync::oneshot::Receiver<Vec<String>>;
    let future: TunnelFuture;

    match args.command {
        Commands::Tcp {
            port,
            remote_port,
            local_addr,
        } => {
            let local_endpoint = parse_socket_addr(&local_addr, port).await?;
            (future, entrypoint_rx) = client.register_tunnel(
                new_tcp_tunnel(TUNNEL_NAME.to_string(), local_endpoint, remote_port),
                shutdown.listen(),
            )?;
        }
        Commands::Udp {
            port,
            remote_port,
            local_addr,
        } => {
            let local_endpoint = parse_socket_addr(&local_addr, port).await?;
            (future, entrypoint_rx) = client.register_tunnel(
                new_udp_tunnel(TUNNEL_NAME.to_string(), local_endpoint, remote_port),
                shutdown.listen(),
            )?;
        }
        Commands::Http {
            port,
            local_addr,
            remote_port,
            subdomain,
            domain,
            random_subdomain,
        } => {
            let local_endpoint = parse_socket_addr(&local_addr, port).await?;
            (future, entrypoint_rx) = client.register_tunnel(
                new_http_tunnel(
                    TUNNEL_NAME.to_string(),
                    local_endpoint,
                    Bytes::from(domain.unwrap_or_default()),
                    Bytes::from(subdomain.unwrap_or_default()),
                    random_subdomain,
                    remote_port.unwrap_or(0),
                ),
                shutdown.listen(),
            )?;
        }
    }

    tokio::spawn(async move {
        if let Err(e) = signal::ctrl_c().await {
            // Something really weird happened. So just panic
            panic!("Failed to listen for the ctrl-c signal: {:?}", e);
        }
        info!("Received ctrl-c signal. Shutting down...");
        shutdown.notify();
    });

    tokio::spawn(async move {
        tokio::select! {
            entrypoints = entrypoint_rx => {
                info!("Entrypoints: {:?}", entrypoints);
            }
        }
    });

    future.await?;

    Ok(())
}

async fn parse_socket_addr(local_addr: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let addr = format!("{}:{}", local_addr, port);
    let mut addrs = addr.to_socket_addrs()?;
    if addrs.len() == 1 {
        return Ok(addrs.next().unwrap());
    }
    let ips = lookup_host(addr).await?.collect::<Vec<_>>();
    if !ips.is_empty() {
        info!(port = port, ips = ?ips, "dns parsed",);

        let mut ip = ips[0];
        ip.set_port(port);
        return Ok(ip);
    }

    Err(anyhow::anyhow!("Invalid address"))
}
