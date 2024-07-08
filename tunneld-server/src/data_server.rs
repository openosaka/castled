use crate::tunnel::http::{DynamicRegistry, FixedRegistry, Http};
use crate::tunnel::tcp::handle_tcp_listener;
use bytes::Bytes;
use std::sync::Arc;
use tokio::{spawn, sync::mpsc};
use tonic::Status;
use tracing::debug;
use tunneld_pkg::shutdown::ShutdownListener;
use tunneld_pkg::{event, util::create_listener};

/// DataServer is responsible for handling the data transfer
/// between user connection and Grpc Server(of Control Server).
pub(crate) struct DataServer {
    _domain: String,
    // tunneld provides a vhttp server for responding requests to the tunnel
    // which is used different subdomains or domains, they still use the same port.
    http_tunnel: Http,
    http_registry: DynamicRegistry,
}

impl DataServer {
    pub(crate) fn new(vhttp_port: u16, domain: String) -> Self {
        let http_registry = DynamicRegistry::new();
        Self {
            _domain: domain,
            http_registry: http_registry.clone(),
            http_tunnel: Http::new(vhttp_port, Arc::new(Box::new(http_registry))),
        }
    }

    pub(crate) async fn listen(
        self,
        shutdown: ShutdownListener,
        mut receiver: mpsc::Receiver<event::ClientEvent>,
    ) -> anyhow::Result<()> {
        let this = Arc::new(self);
        let http_tunnel = this.http_tunnel.clone();
        // start the vhttp tunnel, all the http requests to the vhttp server(with the vhttp_port)
        // has been handled by this http_tunnel.
        http_tunnel.listen(shutdown.clone()).await?;

        while let Some(event) = receiver.recv().await {
            match event.payload {
                event::Payload::RegisterTcp { port } => match create_listener(port).await {
                    Ok(listener) => {
                        let cancel = event.close_listener;
                        let conn_event_chan = event.inbound_events;
                        spawn(async move {
                            handle_tcp_listener(listener, cancel, conn_event_chan.clone()).await;
                            debug!("tcp listener on {} closed", port);
                        });
                        event.resp.send(None).unwrap(); // success
                    }
                    Err(status) => {
                        event.resp.send(Some(status)).unwrap();
                    }
                },
                event::Payload::RegisterHttp {
                    port,
                    subdomain,
                    domain,
                } => {
                    let subdomain_c = subdomain.clone();
                    let domain_c = domain.clone();
                    let resp_status = this
                        .register_http(
                            port,
                            subdomain,
                            domain,
                            event.inbound_events,
                            ShutdownListener::from_cancellation(event.close_listener.clone()),
                        )
                        .await;
                    let this = Arc::clone(&this);
                    if resp_status.is_none() {
                        // means register successfully
                        // listen the close_listener to cancel the unregister domain/subdomain.
                        tokio::spawn(async move {
                            event.close_listener.cancelled().await;
                            if !subdomain_c.is_empty() {
                                this.http_registry.unregister_subdomain(subdomain_c);
                            }
                            if !domain_c.is_empty() {
                                this.http_registry.unregister_domain(domain_c);
                            }
                        });
                    }
                    event.resp.send(resp_status).unwrap();
                }
            }
        }

        debug!("tcp manager quit");
        Ok(())
    }

    async fn register_http(
        &self,
        port: u16,
        subdomain: Bytes,
        domain: Bytes,
        conn_event_chan: mpsc::Sender<event::UserInbound>,
        shutdown: ShutdownListener,
    ) -> Option<Status> {
        if port == 0 && subdomain.is_empty() && domain.is_empty() {
            return Some(Status::invalid_argument("invalid http tunnel arguments"));
        }

        if !subdomain.is_empty() {
            // forward the http request from this subdomain to control server.
            if self.http_registry.subdomain_registered(subdomain.clone()) {
                return Some(Status::already_exists("subdomain already registered"));
            }

            self.http_registry
                .register_subdomain(subdomain, conn_event_chan);
            return None;
        }
        if !domain.is_empty() {
            // forward the http request from this domain to control server.
            if self.http_registry.domain_registered(domain.clone()) {
                return Some(Status::already_exists("domain already registered"));
            }
            self.http_registry
                .register_domain(domain, conn_event_chan.clone());
            return None;
        }
        if port != 0 {
            if let Err(err) = Http::new(
                port,
                Arc::new(Box::new(FixedRegistry::new(conn_event_chan))),
            )
            .listen(shutdown)
            .await
            {
                return Some(Status::internal(err.to_string()));
            };
            return None;
        }

        Some(Status::invalid_argument("invalid http tunnel arguments"))
    }
}
