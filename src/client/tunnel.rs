//! Tunnel configuration for the client.
//! Before starting a tunnel, you need to create a tunnel by this module.
use std::net::SocketAddr;

use bytes::Bytes;

use crate::{
    pb::{self, tunnel, HttpConfig, TcpConfig, UdpConfig},
    socket::{dial_tcp, dial_udp, Dialer},
};

/// Tunnel configuration for the client.
#[derive(Debug)]
pub struct Tunnel<'a> {
    pub(crate) name: &'a str,
    pub(crate) dialer: Dialer,
    pub(crate) config: RemoteConfig<'a>,
}

impl<'a> Tunnel<'a> {
    /// Create a new tunnel.
    pub fn new(name: &'a str, local_endpoint: SocketAddr, config: RemoteConfig<'a>) -> Self {
        Self {
            name,
            dialer: Dialer::new(
                match config {
                    RemoteConfig::Tcp(_) => |endpoint| Box::pin(dial_tcp(endpoint)),
                    RemoteConfig::Udp(_) => |endpoint| Box::pin(dial_udp(endpoint)),
                    RemoteConfig::Http(_) => |endpoint| Box::pin(dial_tcp(endpoint)),
                },
                local_endpoint,
            ),
            config,
        }
    }
}

/// configuration for the http tunnel.
#[derive(Debug, Default)]
pub enum HttpRemoteConfig<'a> {
    Domain(&'a str),
    Subdomain(&'a str),
    RandomSubdomain,
    Port(u16),
    #[default]
    RandomPort,
}

/// Remote configuration for the tunnel.
#[derive(Debug)]
pub enum RemoteConfig<'a> {
    Tcp(u16),
    Udp(u16),
    Http(HttpRemoteConfig<'a>),
}

impl<'a> RemoteConfig<'a> {
    pub(crate) fn to_pb_tunnel(&self, name: &str) -> pb::Tunnel {
        pb::Tunnel {
            name: name.to_string(),
            config: Some(match self {
                Self::Udp(port) => tunnel::Config::Udp(UdpConfig {
                    remote_port: *port as i32,
                }),
                Self::Tcp(port) => tunnel::Config::Tcp(TcpConfig {
                    remote_port: *port as i32,
                }),
                Self::Http(config) => tunnel::Config::Http(config.get_http_config()),
            }),
            ..Default::default()
        }
    }
}

impl HttpRemoteConfig<'_> {
    fn get_http_config(&self) -> HttpConfig {
        match self {
            Self::Domain(domain) => domain_config(Bytes::copy_from_slice(domain.as_bytes())),
            Self::Subdomain(subdomain) => {
                subdomain_config(Bytes::copy_from_slice(subdomain.as_bytes()))
            }
            Self::RandomSubdomain => random_subdomain_config(),
            Self::Port(port) => remote_port_config(*port),
            Self::RandomPort => random_remote_port_config(),
        }
    }
}

fn domain_config(domain: Bytes) -> HttpConfig {
    HttpConfig {
        domain: String::from_utf8_lossy(domain.as_ref()).to_string(),
        random_subdomain: false,
        remote_port: 0,
        subdomain: String::new(),
    }
}

fn subdomain_config(subdomain: Bytes) -> HttpConfig {
    HttpConfig {
        random_subdomain: false,
        domain: String::new(),
        remote_port: 0,
        subdomain: String::from_utf8_lossy(subdomain.as_ref()).to_string(),
    }
}

fn remote_port_config(remote_port: u16) -> HttpConfig {
    HttpConfig {
        remote_port: remote_port as i32,
        random_subdomain: false,
        domain: String::new(),
        subdomain: String::new(),
    }
}

fn random_subdomain_config() -> HttpConfig {
    HttpConfig {
        random_subdomain: true,
        domain: String::new(),
        remote_port: 0,
        subdomain: String::new(),
    }
}

fn random_remote_port_config() -> HttpConfig {
    // all the options are false, so it will be a random remote port
    HttpConfig {
        domain: String::new(),
        remote_port: 0,
        subdomain: String::new(),
        random_subdomain: false,
    }
}
