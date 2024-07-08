use super::{init_data_sender_bridge, BridgeResult};
use anyhow::{Context as _, Result};
use bytes::{BufMut as _, Bytes};
use dashmap::DashMap;
use http::HeaderValue;
use http_body::Frame;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Full, Limited, StreamBody};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{body::Incoming, Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, info_span, Instrument as _};
use tunneld_pkg::bridge::BridgeData;
use tunneld_pkg::shutdown::ShutdownListener;
use tunneld_pkg::util::create_listener;
use tunneld_pkg::{event, get_with_shutdown};

static EMPTY_HOST: HeaderValue = HeaderValue::from_static("");

pub(crate) struct Http {
    port: u16,
    lookup: Arc<Box<dyn LookupRequest>>,
}

impl Clone for Http {
    fn clone(&self) -> Self {
        Self {
            port: self.port,
            lookup: Arc::clone(&self.lookup),
        }
    }
}

impl Http {
    pub(crate) fn new(port: u16, lookup: Arc<Box<dyn LookupRequest>>) -> Self {
        Self { port, lookup }
    }

    pub(crate) async fn serve(self, shutdown: ShutdownListener) -> anyhow::Result<()> {
        let this = Arc::new(self);

        let http1_builder = Arc::new(http1::Builder::new());
        {
            let shutdown = shutdown.clone();
            let vhttp_listener =
                get_with_shutdown!(create_listener(this.port), shutdown.cancelled())?;

            let http1_builder = Arc::clone(&http1_builder);
            let vhttp_handler = async move {
                info!("vhttp server started on port {}", this.port);
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => {
                            return;
                        },
                        Ok((stream, _addr)) = vhttp_listener.accept() => {
                            let this = Arc::clone(&this);
                            let http1_builder = Arc::clone(&http1_builder);

                            tokio::spawn(async move {
                                let io = TokioIo::new(stream);
                                let handler = async move {
                                    let new_service = service_fn(move |req| {
                                        let http_tunnel = this.clone();
                                        async move {
                                            Ok::<Response<BoxBody<Bytes, Infallible>>, hyper::Error>(
                                                http_tunnel.call(req).await,
                                            )
                                        }
                                    });
                                    http1_builder.serve_connection(io, new_service).await
                                }.instrument(info_span!("vhttp_handler"));
                                tokio::task::spawn(handler);
                            });
                        }
                    }
                }
            }
            .instrument(info_span!("vhttp listener"));
            tokio::spawn(vhttp_handler);
        }
        Ok(())
    }

    pub(crate) async fn call(
        &self,
        req: Request<Incoming>,
    ) -> Response<BoxBody<Bytes, Infallible>> {
        let sender = self.lookup.lookup(&req);
        if sender.is_none() {
            return Response::builder()
                .status(404)
                .body(BoxBody::new(Full::new(Bytes::from_static(b"not found"))))
                .unwrap();
        }
        let event_sender = sender.unwrap();
        let bridge = init_data_sender_bridge(event_sender).await.unwrap();

        Self::handle_http_request(req, bridge).await
    }

    async fn handle_http_request(
        req: Request<Incoming>,
        bridge: BridgeResult,
    ) -> Response<BoxBody<Bytes, Infallible>> {
        let raw_req = request_to_bytes(req)
            .await
            .with_context(|| {
                bridge.remove_bridge_sender.cancel();
                "failed to convert request to bytes"
            })
            .unwrap();
        // send the raw request to the tunnel
        bridge
            .data_sender
            .send(raw_req)
            .await
            .with_context(|| {
                bridge.remove_bridge_sender.cancel();
                "failed to send raw request to the tunnel"
            })
            .unwrap();

        // response to user http request by sending data to outbound_rx
        let (outbound_tx, outbound_rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(1024);

        let mut data_receiver = bridge.data_receiver;

        // read the response from the tunnel and send it back to the user
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = bridge.client_cancel_receiver.cancelled() => {
                        break;
                    }
                    Some(data) = data_receiver.recv() => {
                        match data {
                            BridgeData::Data(data) => {
                                if data.is_empty() {
                                    // means no more data
                                    break;
                                }
                                let frame = Frame::data(Bytes::from(data));
                                let _ = outbound_tx.send(Ok(frame)).await;
                            }
                            _ => {
                                panic!("unexpected data type");
                            }
                        }
                    }
                }
            }
            bridge.remove_bridge_sender.cancel();
        });

        let stream = ReceiverStream::new(outbound_rx);
        let body = BoxBody::new(StreamBody::new(stream));
        Response::builder().body(body).unwrap()
    }
}

// TODO(sword): use stream
async fn request_to_bytes(req: Request<Incoming>) -> Result<Vec<u8>> {
    let (parts, body) = req.into_parts();
    let mut buf = vec![].writer();

    // e.g. GET / HTTP/1.1
    write!(
        buf,
        "{} {} {:?}\r\n",
        parts.method, parts.uri, parts.version
    )?;
    for (key, value) in parts.headers.iter() {
        write!(buf, "{}: {}\r\n", key, value.to_str().unwrap())?;
    }
    let _ = buf.write(b"\r\n")?;

    // TODO(sword): convert to stream
    let body = Limited::new(body, 1024 * 1024)
        .collect()
        .await
        .map_err(|_| anyhow::anyhow!("failed to collect body"))?
        .to_bytes();
    buf.write(body.to_vec().as_slice())
        .context("failed to convert body to bytes")?;

    Ok(buf.into_inner())
}

/// LookupRequest is a trait that provides a method to
/// lookup the request and return the sender to send the data.
pub(crate) trait LookupRequest: Send + Sync {
    fn lookup(&self, req: &Request<Incoming>) -> Option<mpsc::Sender<event::UserIncoming>>;
}

/// FixedRegistry is a registry that always returns the fixed sender.
#[derive(Clone)]
pub(crate) struct FixedRegistry {
    sender: mpsc::Sender<event::UserIncoming>,
}

impl FixedRegistry {
    pub(crate) fn new(sender: mpsc::Sender<event::UserIncoming>) -> Self {
        Self { sender }
    }
}

impl LookupRequest for FixedRegistry {
    fn lookup(&self, _: &Request<Incoming>) -> Option<mpsc::Sender<event::UserIncoming>> {
        Some(self.sender.clone())
    }
}

/// DynamicRegistry is a registry that can register and unregister the domain and subdomain.
#[derive(Clone, Default)]
pub(crate) struct DynamicRegistry {
    domains: Arc<DashMap<Bytes, mpsc::Sender<event::UserIncoming>>>,
    subdomains: Arc<DashMap<Bytes, mpsc::Sender<event::UserIncoming>>>,
}

impl LookupRequest for DynamicRegistry {
    fn lookup(&self, req: &Request<Incoming>) -> Option<mpsc::Sender<event::UserIncoming>> {
        let host = req.headers().get("host").unwrap_or(&EMPTY_HOST);
        let host = host.to_str().unwrap_or_default();
        debug!("host: {}", host);
        // match the host
        let result = self.get_domain(Bytes::copy_from_slice(host.as_bytes()));
        if result.is_some() {
            return result;
        }

        // match subdomain
        let subdomain = host.split('.').next().unwrap_or_default();
        let subdomain = Bytes::copy_from_slice(subdomain.as_bytes());
        self.get_subdomain(subdomain)
    }
}

impl DynamicRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_domain(
        &self,
        domain: Bytes,
        conn_event_chan: mpsc::Sender<event::UserIncoming>,
    ) {
        self.domains.insert(domain, conn_event_chan);
    }

    pub(crate) fn unregister_domain(&self, domain: Bytes) {
        self.domains.remove(&domain);
    }

    pub(crate) fn domain_registered(&self, domain: Bytes) -> bool {
        self.domains.contains_key(&domain)
    }

    pub(crate) fn get_domain(&self, domain: Bytes) -> Option<mpsc::Sender<event::UserIncoming>> {
        self.domains.get(&domain).map(|x| x.value().clone())
    }

    pub(crate) fn subdomain_registered(&self, subdomain: Bytes) -> bool {
        self.subdomains.contains_key(&subdomain)
    }

    pub(crate) fn unregister_subdomain(&self, subdomain: Bytes) {
        self.subdomains.remove(&subdomain);
    }

    pub(crate) fn get_subdomain(
        &self,
        subdomain: Bytes,
    ) -> Option<mpsc::Sender<event::UserIncoming>> {
        self.subdomains.get(&subdomain).map(|x| x.value().clone())
    }

    pub(crate) fn register_subdomain(
        &self,
        subdomain: Bytes,
        conn_event_chan: mpsc::Sender<event::UserIncoming>,
    ) {
        self.subdomains.insert(subdomain, conn_event_chan);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_the_cloned_http_shares_same_registrations() {
        let http1 = DynamicRegistry::new();
        let http2 = http1.clone();

        let (tx1, mut rx1) = mpsc::channel(1);
        http1.register_domain(Bytes::from_static(b"example1.com"), tx1.clone());
        http2.register_domain(Bytes::from_static(b"example2.com"), tx1.clone());

        let (tx2, mut rx2) = mpsc::channel(1);
        http1.register_subdomain(Bytes::from_static(b"foo"), tx2.clone());
        http2.register_subdomain(Bytes::from_static(b"bar"), tx2.clone());

        assert!(http1.domain_registered(Bytes::from_static(b"example2.com")));
        assert!(http2.domain_registered(Bytes::from_static(b"example1.com")));
        assert!(http1.subdomain_registered(Bytes::from_static(b"bar")));
        assert!(http2.subdomain_registered(Bytes::from_static(b"foo")));

        assert!(http1
            .get_domain(Bytes::from_static(b"example2.com"))
            .unwrap()
            .send(event::UserIncoming::Remove(Bytes::from_static(
                b"example2.com",
            )))
            .await
            .is_ok());
        let received = rx1.recv().await;
        assert!(received.is_some());

        assert!(http2
            .get_subdomain(Bytes::from_static(b"foo"))
            .unwrap()
            .send(event::UserIncoming::Remove(Bytes::from_static(b"foo.com")))
            .await
            .is_ok());
        let received = rx2.recv().await;
        assert!(received.is_some());

        http2.unregister_subdomain(Bytes::from_static(b"foo"));
        assert!(!http1.subdomain_registered(Bytes::from_static(b"foo")));
        assert!(http1.subdomain_registered(Bytes::from_static(b"bar")));

        http1.unregister_domain(Bytes::from_static(b"example2.com"));
        assert!(!http2.domain_registered(Bytes::from_static(b"example2.com")));
        assert!(http2.domain_registered(Bytes::from_static(b"example1.com")));
    }
}
