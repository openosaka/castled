mod control_server;
mod data_server;
mod tunnel;
pub use control_server::Server;
use tunneld_pkg::event;

use std::net::IpAddr;
use std::ops::RangeInclusive;

#[derive(Debug)]
pub struct Config {
    /// control_port is the port of the control server.
    ///
    /// the client will connect to this port to register a tunnel.
    pub control_port: u16,
    pub vhttp_port: u16,
    pub entrypoint: EntrypointConfig,
}

#[derive(Debug)]
pub struct EntrypointConfig {
    pub domain: Vec<String>,
    pub ip: Vec<IpAddr>,
    pub vhttp_behind_proxy_tls: bool,
    pub port_range: RangeInclusive<u16>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            control_port: 6610,
            vhttp_port: 6611,
            entrypoint: Default::default(),
        }
    }
}

impl Default for EntrypointConfig {
    fn default() -> Self {
        Self {
            domain: Vec::new(),
            ip: Vec::new(),
            vhttp_behind_proxy_tls: false,
            port_range: 1024..=65535,
        }
    }
}

impl EntrypointConfig {
    pub(crate) fn make_entrypoint(&self, payload: &event::Payload, port: u16) -> Vec<String> {
        let mut entrypoints: Vec<String> = Vec::new();

        let uri_parts = self.get_uri_parts(payload);

        for host in &uri_parts.host {
            let entrypoint = if uri_parts.include_port {
                format!("{}://{}:{}", uri_parts.scheme, host, port)
            } else {
                format!("{}://{}", uri_parts.scheme, host)
            };
            entrypoints.push(entrypoint.to_string());
        }

        entrypoints
    }

    fn get_uri_parts<'a>(&'a self, payload: &'a event::Payload) -> PartsOfUri {
        match payload {
            event::Payload::RegisterTcp { .. } => self.make_parts_of_uri("tcp"),
            event::Payload::RegisterUdp { .. } => self.make_parts_of_uri("udp"),
            event::Payload::RegisterHttp {
                subdomain, domain, ..
            } => {
                if !domain.is_empty() || !subdomain.is_empty() {
                    // if the vhttp server is behind a http proxy like nginx, set this to true.
                    // also we assume the client registers the tunnel with the domain or subdomain.
                    //
                    // Note: actually proxy like caddy proxy support port range, so we actually can support
                    // https on any remote port, for now, we assume the proxy https port is 443.
                    let schema = if self.vhttp_behind_proxy_tls {
                        "https"
                    } else {
                        "http"
                    };

                    if !domain.is_empty() {
                        let custom_domain =
                            Box::new(String::from_utf8_lossy(domain).into_owned()).into_boxed_str();
                        PartsOfUri {
                            scheme: schema,
                            include_port: false,
                            host: vec![Box::leak(custom_domain)],
                        }
                    } else {
                        let mut host: Vec<&str> = Vec::new();
                        for domain in &self.domain {
                            let domain =
                                format!("{}.{}", String::from_utf8_lossy(subdomain), domain);
                            host.push(Box::leak(domain.to_string().into_boxed_str()));
                        }
                        PartsOfUri {
                            scheme: schema,
                            include_port: false,
                            host,
                        }
                    }
                } else {
                    self.make_parts_of_uri("http")
                }
            }
        }
    }

    fn make_parts_of_uri<'a>(&'a self, schema: &'a str) -> PartsOfUri {
        let mut host: Vec<&str> = Vec::new();
        for ip in &self.ip {
            host.push(Box::leak(ip.to_string().into_boxed_str()));
        }
        for domain in &self.domain {
            host.push(Box::leak(domain.to_string().into_boxed_str()));
        }
        PartsOfUri {
            scheme: schema,
            include_port: true,
            host,
        }
    }
}

struct PartsOfUri<'a> {
    scheme: &'a str,
    include_port: bool,
    host: Vec<&'a str>,
}
