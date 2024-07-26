use crate::event::{self, ClientEventResponse, Payload};

use super::{
    tunnel::{
        create_socket,
        http::{DynamicRegistry, FixedRegistry, Http},
        tcp::Tcp,
        udp::Udp,
    },
    EntrypointConfig,
};
use async_shutdown::ShutdownSignal;
use bytes::Bytes;
use std::sync::Arc;
use tokio::{
    net::{TcpListener, UdpSocket},
    spawn,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::{error, info};

/// DataServer is responsible for handling the data transfer
/// between user connection and Grpc Server(of Control Server).
pub(crate) struct DataServer {
    // castled provides a vhttp server for responding requests to the tunnel
    // which is used different subdomains or domains, they still use the same port.
    http_tunnel: Http,
    http_registry: DynamicRegistry,
    entrypoint_config: EntrypointConfig,
}

impl DataServer {
    pub(crate) fn new(vhttp_port: u16, entrypoint_config: EntrypointConfig) -> Self {
        let http_registry = DynamicRegistry::new();
        Self {
            http_registry: http_registry.clone(),
            http_tunnel: Http::new(vhttp_port, Arc::new(Box::new(http_registry))),
            entrypoint_config,
        }
    }

    pub(crate) async fn listen(
        self,
        shutdown: ShutdownSignal<()>,
        mut receiver: mpsc::Receiver<event::ClientEvent>,
    ) -> anyhow::Result<()> {
        let this = Arc::new(self);
        let http_tunnel = this.http_tunnel.clone();
        // start the vhttp tunnel, all the http requests to the vhttp server(with the vhttp_port)
        // will be handled by this http_tunnel.
        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();
        let shutdown_listener = shutdown.clone();
        tokio::spawn(async move {
            shutdown_listener.await;
            cancel_w.cancel();
        });
        http_tunnel.serve(cancel).await?;

        while let Some(event) = receiver.recv().await {
            match event.payload {
                event::Payload::RegisterTcp { port } => {
                    let result: Result<(u16, TcpListener), tonic::Status> =
                        create_socket::<Tcp>(port, this.entrypoint_config.port_range.clone()).await;
                    match result {
                        Ok((port, listener)) => {
                            let cancel = event.close_listener;
                            let conn_event_chan = event.incoming_events;
                            spawn(async move {
                                Tcp::new(listener, conn_event_chan.clone())
                                    .serve(cancel)
                                    .await;
                                info!("tcp closed on {}", port);
                            });
                            event
                                .resp
                                .send(ClientEventResponse::registered(
                                    this.entrypoint_config.make_entrypoint(&event.payload, port),
                                ))
                                .unwrap(); // success
                        }
                        Err(status) => {
                            event
                                .resp
                                .send(ClientEventResponse::registered_failed(status))
                                .unwrap();
                        }
                    }
                }
                event::Payload::RegisterUdp { port } => {
                    let result: Result<(u16, UdpSocket), tonic::Status> =
                        create_socket::<Udp>(port, this.entrypoint_config.port_range.clone()).await;

                    match result {
                        Ok((port, socket)) => {
                            let socket = socket;
                            let cancel = event.close_listener;
                            let conn_event_chan = event.incoming_events;
                            spawn(async move {
                                Udp::new(socket, conn_event_chan.clone())
                                    .serve(cancel)
                                    .await;
                                info!("udp socket on {} closed", port);
                            });
                            event
                                .resp
                                .send(ClientEventResponse::registered(
                                    this.entrypoint_config.make_entrypoint(&event.payload, port),
                                ))
                                .unwrap(); // success
                        }
                        Err(status) => {
                            event
                                .resp
                                .send(ClientEventResponse::registered_failed(status))
                                .unwrap();
                        }
                    }
                }
                event::Payload::RegisterHttp {
                    mut port,
                    mut subdomain,
                    domain,
                    random_subdomain,
                } => {
                    let subdomain_c = subdomain.clone();
                    let domain_c = domain.clone();
                    let domain_c2 = domain.clone();
                    let resp_status = shutdown
                        .wrap_cancel(this.register_http(
                            event.close_listener.clone(),
                            domain,
                            &mut subdomain,
                            random_subdomain,
                            &mut port,
                            event.incoming_events,
                        ))
                        .await;
                    let this = Arc::clone(&this);
                    if let Ok(Some(status)) = resp_status {
                        event
                            .resp
                            .send(ClientEventResponse::registered_failed(status))
                            .unwrap();
                    } else {
                        // means register successfully
                        // listen the close_listener to cancel the unregister domain/subdomain.
                        let payload = Payload::RegisterHttp {
                            port,
                            subdomain,
                            domain: domain_c2,
                            random_subdomain,
                        };
                        event
                            .resp
                            .send(ClientEventResponse::registered(
                                this.entrypoint_config.make_entrypoint(&payload, port),
                            ))
                            .unwrap();

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
                }
            }
        }

        info!("tcp manager quit");
        Ok(())
    }

    async fn register_http(
        &self,
        shutdown: CancellationToken,
        domain: Bytes,
        subdomain: &mut Bytes,
        random_subdomain: bool,
        port: &mut u16,
        conn_event_chan: mpsc::Sender<event::UserIncoming>,
    ) -> Option<Status> {
        if !domain.is_empty() {
            // forward the http request from this domain to control server.
            if self.http_registry.domain_registered(&domain) {
                return Some(Status::already_exists("domain already registered"));
            }
            self.http_registry
                .register_domain(domain, conn_event_chan.clone());
            return None;
        }

        if subdomain.is_empty() && random_subdomain {
            loop {
                let subdomain2 = Bytes::from(generate_random_subdomain(8));
                if !self.http_registry.subdomain_registered(&subdomain2) {
                    *subdomain = subdomain2;
                    break;
                }
            }
        }

        if !subdomain.is_empty() {
            // forward the http request from this subdomain to control server.
            if self.http_registry.subdomain_registered(subdomain) {
                return Some(Status::already_exists("subdomain already registered"));
            }

            info!("subdomain registered: {:?}", subdomain);
            self.http_registry
                .register_subdomain(subdomain.clone(), conn_event_chan);
            return None;
        }

        if *port != 0 {
            if let Err(err) = Http::new(
                *port,
                Arc::new(Box::new(FixedRegistry::new(conn_event_chan))),
            )
            .serve(shutdown)
            .await
            {
                error!(port = *port, err = ?err, "failed to start http server");
                Some(Status::internal(err.to_string()))
            } else {
                info!(port = *port, "http server started");
                None
            }
        } else {
            let result =
                create_socket::<Tcp>(*port, self.entrypoint_config.port_range.clone()).await;
            match result {
                Ok((random_port, listener)) => {
                    *port = random_port;
                    let conn_event_chan = conn_event_chan.clone();
                    spawn(async move {
                        Http::new(
                            random_port,
                            Arc::new(Box::new(FixedRegistry::new(conn_event_chan))),
                        )
                        .serve_with_listener(listener, shutdown);
                    });
                    None
                }
                Err(err) => Some(err),
            }
        }
    }
}

fn generate_random_subdomain(length: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = fastrand::Rng::new();
    let subdomain: String = (0..length)
        .map(|_| {
            let idx = rng.u8(..CHARSET.len() as u8) as usize;
            CHARSET[idx] as char
        })
        .collect();
    subdomain
}
