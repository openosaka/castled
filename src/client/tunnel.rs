use std::net::SocketAddr;

use bytes::Bytes;

use crate::protocol::pb::{
    self,
    tunnel::{self, Type},
    HttpConfig, TcpConfig, UdpConfig,
};

pub struct Tunnel {
    pub(crate) inner: pb::Tunnel,
    pub(crate) local_endpoint: SocketAddr,
}

pub fn new_tcp_tunnel(name: String, local_endpoint: SocketAddr, remote_port: u16) -> Tunnel {
    Tunnel {
        inner: pb::Tunnel {
            name,
            r#type: Type::Tcp as i32,
            config: Some(tunnel::Config::Tcp(TcpConfig {
                remote_port: remote_port as i32,
            })),
            ..Default::default()
        },
        local_endpoint,
    }
}

pub fn new_udp_tunnel(name: String, local_endpoint: SocketAddr, remote_port: u16) -> Tunnel {
    Tunnel {
        inner: pb::Tunnel {
            name,
            r#type: Type::Udp as i32,
            config: Some(tunnel::Config::Udp(UdpConfig {
                remote_port: remote_port as i32,
            })),
            ..Default::default()
        },
        local_endpoint,
    }
}

pub fn new_http_tunnel(
    name: String,
    local_endpoint: SocketAddr,
    domain: Bytes,
    subdomain: Bytes,
    random_subdomain: bool,
    remote_port: u16,
) -> Tunnel {
    Tunnel {
        inner: pb::Tunnel {
            name,
            r#type: Type::Http as i32,
            config: Some(tunnel::Config::Http(get_http_config(
                domain,
                subdomain,
                random_subdomain,
                remote_port,
            ))),
            ..Default::default()
        },
        local_endpoint,
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

fn get_http_config(
    domain: Bytes,
    subdomain: Bytes,
    random_subdomain: bool,
    remote_port: u16,
) -> HttpConfig {
    if !domain.is_empty() {
        domain_config(domain)
    } else if !subdomain.is_empty() {
        subdomain_config(subdomain)
    } else if remote_port != 0 {
        remote_port_config(remote_port)
    } else if random_subdomain {
        random_subdomain_config()
    } else {
        random_remote_port_config()
    }
}
