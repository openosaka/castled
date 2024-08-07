use async_shutdown::ShutdownManager;
use castled::{
    client::{
        tunnel::{HttpRemoteConfig, RemoteConfig, Tunnel},
        Client,
    },
    debug::setup_logging,
};
use clap::{Parser, Subcommand};
use std::net::{SocketAddr, ToSocketAddrs};
use tokio::{net::lookup_host, signal};
use tracing::info;

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
        local_host: String,
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
        local_host: String,
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
        local_host: String,
    },
}

const DEFAULT_TCP_TUNNEL_NAME: &str = "castle-tcp";
const DEFAULT_UDP_TUNNEL_NAME: &str = "castle-udp";
const DEFAULT_HTTP_TUNNEL_NAME: &str = "castle-http";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 6670 is the default tokio console server port of the client,
    // use `TOKIO_CONSOLE_BIND=127.0.0.1:6669` to change it.
    setup_logging(6670);

    let args = Args::parse();

    let client = Client::new(args.server_addr).await?;
    let tunnel;
    let shutdown: ShutdownManager<i8> = ShutdownManager::new();
    let wait_complete = shutdown.wait_shutdown_complete();

    match args.command {
        Commands::Tcp {
            port,
            remote_port,
            local_host,
        } => {
            let local_endpoint = parse_socket_addr(&local_host, port).await?;
            tunnel = Tunnel::new(
                DEFAULT_TCP_TUNNEL_NAME,
                local_endpoint,
                RemoteConfig::Tcp(remote_port),
            );
        }
        Commands::Udp {
            port,
            remote_port,
            local_host,
        } => {
            let local_endpoint = parse_socket_addr(&local_host, port).await?;
            tunnel = Tunnel::new(
                DEFAULT_UDP_TUNNEL_NAME,
                local_endpoint,
                RemoteConfig::Udp(remote_port),
            );
        }
        Commands::Http {
            port,
            local_host,
            domain,
            subdomain,
            random_subdomain,
            remote_port,
        } => {
            let local_endpoint = parse_socket_addr(&local_host, port).await?;
            tunnel = Tunnel::new(
                DEFAULT_HTTP_TUNNEL_NAME,
                local_endpoint,
                RemoteConfig::Http(if domain.is_some() {
                    HttpRemoteConfig::Domain(to_str(domain.unwrap()))
                } else if subdomain.is_some() {
                    HttpRemoteConfig::Subdomain(to_str(subdomain.unwrap()))
                } else if random_subdomain {
                    HttpRemoteConfig::RandomSubdomain
                } else if remote_port.is_some() {
                    HttpRemoteConfig::Port(remote_port.unwrap())
                } else {
                    HttpRemoteConfig::RandomPort
                }),
            );
        }
    }

    let entrypoint = client.start_tunnel(tunnel, shutdown.clone()).await?;

    info!("Entrypoint: {:?}", entrypoint);

    tokio::spawn(async move {
        if let Err(e) = signal::ctrl_c().await {
            // Something really weird happened. So just panic
            panic!("Failed to listen for the ctrl-c signal: {:?}", e);
        }
        info!("Received ctrl-c signal. Shutting down...");
        shutdown.trigger_shutdown(0).ok();
    });

    let code = wait_complete.await;
    std::process::exit(code as i32)
}

fn to_str<'a>(s: String) -> &'a str {
    Box::leak(s.into_boxed_str())
}

async fn parse_socket_addr(local_host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let addr = format!("{}:{}", local_host, port);
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
