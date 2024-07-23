use std::net::SocketAddr;

use bytes::Bytes;

use crate::pb::{self, tunnel, HttpConfig, TcpConfig, UdpConfig};

pub struct Tunnel<'a> {
    pub(crate) name: &'a str,
    pub(crate) local_endpoint: SocketAddr,
    pub(crate) config: RemoteConfig<'a>,
}

impl<'a> Tunnel<'a> {
    pub fn new(name: &'a str, local_endpoint: SocketAddr, config: RemoteConfig<'a>) -> Self {
        Self {
            name,
            local_endpoint,
            config,
        }
    }
}

pub enum HttpRemoteConfig<'a> {
    Domain(&'a str),
    Subdomain(&'a str),
    RandomSubdomain,
    Port(u16),
    RandomPort,
}

pub enum RemoteConfig<'a> {
    Udp(u16),
    Tcp(u16),
    Http(HttpRemoteConfig<'a>),
}

impl<'a> RemoteConfig<'a> {
    pub(crate) fn to_pb_tunnel(&self, name: &str) -> pb::Tunnel {
        let mut pb_tunnel = pb::Tunnel::default();
        pb_tunnel.name = name.to_string();

        match self {
            Self::Udp(port) => {
                pb_tunnel.config = Some(tunnel::Config::Udp(UdpConfig {
                    remote_port: *port as i32,
                }));
            }
            Self::Tcp(port) => {
                pb_tunnel.config = Some(tunnel::Config::Tcp(TcpConfig {
                    remote_port: *port as i32,
                }));
            }
            Self::Http(config) => {
                pb_tunnel.config = Some(tunnel::Config::Http(config.get_http_config()));
            }
        }

        pb_tunnel
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

impl Default for HttpRemoteConfig<'_> {
    fn default() -> Self {
        HttpRemoteConfig::RandomPort
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
