use crate::{
    event::{self, ClientEventResponse, Payload},
    server::port::{Available, PortManager},
    socket::create_tcp_listener,
};

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
use rand::prelude::*;
use rand::rngs::StdRng;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, UdpSocket},
    spawn,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::info;

/// DataServer is responsible for handling the data transfer
/// between user connection and Grpc Server(of Control Server).
pub(crate) struct DataServer {
    vhttp_port: u16,
    http_registry: DynamicRegistry,
    entrypoint_config: EntrypointConfig,
    port_manager: PortManager,
}

impl DataServer {
    pub(crate) fn new(vhttp_port: u16, entrypoint_config: EntrypointConfig) -> Self {
        let http_registry = DynamicRegistry::new();
        let port_manager = PortManager::new(
            entrypoint_config.port_range.clone(),
            entrypoint_config.exclude_ports.clone(),
        );
        Self {
            vhttp_port,
            http_registry,
            port_manager,
            entrypoint_config,
        }
    }

    pub(crate) async fn listen(
        self,
        shutdown: ShutdownSignal<()>,
        mut receiver: mpsc::Receiver<event::ClientEvent>,
    ) -> anyhow::Result<()> {
        let this = Arc::new(self);

        // start the vhttp tunnel, all the http requests to the vhttp server(with the vhttp_port)
        // will be handled by this http_tunnel.
        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();
        let shutdown_listener = shutdown.clone();
        tokio::spawn(async move {
            shutdown_listener.await;
            cancel_w.cancel();
        });

        let http_tunnel = Http::new(Arc::new(Box::new(this.http_registry.clone())));
        let tcp_listener = create_tcp_listener(this.vhttp_port).await?;
        tokio::spawn(async move {
            http_tunnel.serve_with_listener(tcp_listener, cancel).await;
        });

        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let seed = since_the_epoch.as_secs();
        let mut rng = StdRng::seed_from_u64(seed);

        loop {
            tokio::select! {
                _ = shutdown.clone() => {
                    break;
                }
                event = receiver.recv() => {
                    let event = match event {
                        Some(event) => event,
                        None => break,
                    };
                    match event.payload {
                        event::Payload::RegisterTcp { port } => {
                            let result: Result<(Available, TcpListener), tonic::Status> =
                                create_socket::<Tcp>(port, &mut this.port_manager.clone()).await;
                            match result {
                                Ok((available_port, listener)) => {
                                    let cancel = event.close_listener;
                                    let conn_event_chan = event.incoming_events;
                                    event
                                        .resp
                                        .send(ClientEventResponse::registered(
                                            this.entrypoint_config
                                                .make_entrypoint(&event.payload, *available_port),
                                        ))
                                        .unwrap(); // success
                                    spawn(async move {
                                        Tcp::new(listener, conn_event_chan.clone())
                                            .serve(cancel)
                                            .await;
                                        info!(port = *available_port, "tcp server closed");
                                    });
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
                            let result: Result<(Available, UdpSocket), tonic::Status> =
                                create_socket::<Udp>(port, &mut this.port_manager.clone()).await;

                            match result {
                                Ok((available_port, socket)) => {
                                    let socket = socket;
                                    let cancel = event.close_listener;
                                    let conn_event_chan = event.incoming_events;
                                    event
                                        .resp
                                        .send(ClientEventResponse::registered(
                                            this.entrypoint_config
                                                .make_entrypoint(&event.payload, *available_port),
                                        ))
                                        .unwrap(); // success
                                    spawn(async move {
                                        Udp::new(socket, conn_event_chan.clone())
                                            .serve(cancel)
                                            .await;
                                        info!(port = *available_port, "udp server closed");
                                    });
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
                                    &mut rng,
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
            }
        }

        info!("data server quit");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn register_http(
        &self,
        shutdown: CancellationToken,
        domain: Bytes,
        subdomain: &mut Bytes,
        random_subdomain: bool,
        port: &mut u16,
        conn_event_chan: mpsc::Sender<event::UserIncoming>,
        rng: &mut StdRng,
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
                let subdomain2 = Bytes::from(self.generate_random_subdomain(8, rng).await);
                if !self.http_registry.subdomain_registered(&subdomain2) {
                    *subdomain = subdomain2;
                    break;
                }
            }
            info!(?subdomain, "random subdomain generated");
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
            match create_socket::<Tcp>(*port, &mut self.port_manager.clone()).await {
                Ok((available_port, listener)) => {
                    spawn(async move {
                        info!(port = *available_port, "http server started");
                        Http::new(Arc::new(Box::new(FixedRegistry::new(conn_event_chan))))
                            .serve_with_listener(listener, shutdown)
                            .await;
                    });
                    None
                }
                Err(status) => Some(status),
            }
        } else {
            let result = create_socket::<Tcp>(*port, &mut self.port_manager.clone()).await;
            match result {
                Ok((available_port, listener)) => {
                    *port = *available_port;
                    let conn_event_chan = conn_event_chan.clone();
                    spawn(async move {
                        Http::new(Arc::new(Box::new(FixedRegistry::new(conn_event_chan))))
                            .serve_with_listener(listener, shutdown)
                            .await;
                        drop(available_port);
                    });
                    None
                }
                Err(err) => Some(err),
            }
        }
    }

    async fn generate_random_subdomain(&self, length: usize, rng: &mut StdRng) -> String {
        static CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let subdomain: String = (0..length)
            .map(|_| {
                let idx: usize = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();
        subdomain
    }
}

#[cfg(test)]
mod test {
    use async_shutdown::ShutdownManager;
    use tokio::time::sleep;

    use crate::debug;

    use super::*;

    #[tokio::test]
    async fn test_cannot_listen_on_same_vhttp_port() {
        // debug::setup_logging(6669); // enable for debug
        let (_t1, r1) = mpsc::channel(10);
        let shutdown1 = ShutdownManager::new();
        let signal1 = shutdown1.wait_shutdown_triggered();
        let s1 = DataServer::new(3100, EntrypointConfig::default());
        let h1 = tokio::spawn(async move { s1.listen(signal1, r1).await });

        sleep(std::time::Duration::from_millis(10)).await;

        let (_t2, r2) = mpsc::channel(10);
        let s2 = DataServer::new(3100, EntrypointConfig::default());
        let shutdown2 = ShutdownManager::new();
        let h2 = s2.listen(shutdown2.wait_shutdown_triggered(), r2).await;
        assert!(h2.is_err());
        shutdown1.trigger_shutdown(()).unwrap();
        let h1 = tokio::join!(h1);
        assert!(h1.0.is_ok());
        shutdown2.trigger_shutdown(()).unwrap();
    }
}
