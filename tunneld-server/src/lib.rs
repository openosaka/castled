mod control_server;
mod data_server;
mod tunnel;

pub use control_server::Server;
use tunneld_protocol::pb::RegisterReq;

#[derive(Debug, Default)]
pub struct Config {
    /// control_port is the port of the control server.
    ///
    /// the client will connect to this port to register a tunnel.
    pub control_port: u16,
    pub vhttp_port: u16,
    pub domain: Vec<String>,
    pub ip: Vec<IpAddr>,
    pub vhttp_behind_proxy_tls: bool,
}

use std::net::IpAddr;
use tunneld_protocol::pb::tunnel::Config as TunnelConfig;
use tunneld_protocol::pb::{tunnel::Config::Http, tunnel::Config::Tcp, tunnel::Config::Udp};

impl Config {
    pub(crate) fn make_entrypoint<'a>(&self, req: &RegisterReq) -> Vec<&'a str> {
        let mut entrypoints: Vec<&str> = Vec::new();

        let config = req.tunnel.as_ref().unwrap().config.as_ref().unwrap();
        let uri_parts = self.get_uri_parts(config);

        for host in &uri_parts.host {
            let entrypoint = if uri_parts.include_port {
                format!("{}://{}:{}", uri_parts.scheme, host, uri_parts.port)
            } else {
                format!("{}://{}", uri_parts.scheme, host)
            };
            entrypoints.push(Box::leak(entrypoint.into_boxed_str()));
        }

        entrypoints
    }

    fn get_uri_parts<'a>(&'a self, config: &'a TunnelConfig) -> PartsOfUri {
        match config {
            Tcp(tcp) => self.make_entrypoint_with_schema_and_port("tcp", tcp.remote_port as u16),
            Http(http) => {
                if !http.domain.is_empty() || !http.subdomain.is_empty() {
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

                    if !http.domain.is_empty() {
                        PartsOfUri {
                            scheme: schema,
                            include_port: false,
                            port: 0, // we don't care about the port here.
                            host: vec![http.domain.as_str()],
                        }
                    } else {
                        let mut host: Vec<&str> = Vec::new();
                        for domain in &self.domain {
                            let domain = format!("{}.{}", http.subdomain, domain);
                            host.push(Box::leak(domain.to_string().into_boxed_str()));
                        }
                        PartsOfUri {
                            scheme: schema,
                            include_port: false,
                            port: 0, // we don't care about the port here.
                            host,
                        }
                    }
                } else {
                    self.make_entrypoint_with_schema_and_port("http", http.remote_port as u16)
                }
            }
            Udp(udp) => self.make_entrypoint_with_schema_and_port("udp", udp.remote_port as u16),
        }
    }

    fn make_entrypoint_with_schema_and_port<'a>(
        &'a self,
        schema: &'a str,
        port: u16,
    ) -> PartsOfUri {
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
            port,
            host,
        }
    }
}

struct PartsOfUri<'a> {
    scheme: &'a str,
    include_port: bool,
    port: u16,
    host: Vec<&'a str>,
}
