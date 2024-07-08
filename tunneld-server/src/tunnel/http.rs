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
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, info_span, Instrument as _};
use tunneld_pkg::shutdown::ShutdownListener;
use tunneld_pkg::util::create_listener;
use tunneld_pkg::{event, get_with_shutdown};
use uuid::Uuid;

static EMPTY_HOST: HeaderValue = HeaderValue::from_static("");

#[derive(Clone, Default)]
pub(crate) struct Http {
    port: u16,
    pub domains: Arc<DashMap<Bytes, mpsc::Sender<event::ConnEvent>>>,
    subdomains: Arc<DashMap<Bytes, mpsc::Sender<event::ConnEvent>>>,
}

impl Http {
    pub(crate) fn new(port: u16) -> Self {
        Self {
            port,
            ..Self::default()
        }
    }

    pub(crate) async fn listen(self, shutdown: ShutdownListener) -> anyhow::Result<()> {
        let this = Arc::new(self);

        let http1_builder = Arc::new(http1::Builder::new());
        {
            let vhttp_listener = get_with_shutdown!(create_listener(this.port), shutdown.clone())?;

            let http1_builder = Arc::clone(&http1_builder);
            let vhttp_handler = async move {
                info!("vhttp server started on port {}", this.port);
                loop {
                    tokio::select! {
                        _ = shutdown.clone() => {},
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

    pub(crate) fn register_domain(
        &self,
        domain: Bytes,
        conn_event_chan: mpsc::Sender<event::ConnEvent>,
    ) {
        self.domains.insert(domain, conn_event_chan);
    }

    pub(crate) fn unregister_domain(&self, domain: Bytes) {
        self.domains.remove(&domain);
    }

    pub(crate) fn domain_registered(&self, domain: Bytes) -> bool {
        self.domains.contains_key(&domain)
    }

    pub(crate) fn get_domain(&self, domain: Bytes) -> Option<mpsc::Sender<event::ConnEvent>> {
        self.domains.get(&domain).map(|x| x.value().clone())
    }

    pub(crate) fn subdomain_registered(&self, subdomain: Bytes) -> bool {
        self.subdomains.contains_key(&subdomain)
    }

    pub(crate) fn unregister_subdomain(&self, subdomain: Bytes) {
        self.subdomains.remove(&subdomain);
    }

    pub(crate) fn get_subdomain(&self, subdomain: Bytes) -> Option<mpsc::Sender<event::ConnEvent>> {
        self.subdomains.get(&subdomain).map(|x| x.value().clone())
    }

    pub(crate) fn register_subdomain(
        &self,
        subdomain: Bytes,
        conn_event_chan: mpsc::Sender<event::ConnEvent>,
    ) {
        self.subdomains.insert(subdomain, conn_event_chan);
    }

    pub(crate) async fn call(
        &self,
        req: Request<Incoming>,
    ) -> Response<BoxBody<Bytes, Infallible>> {
        let host = req.headers().get("host").unwrap_or(&EMPTY_HOST);
        let host = host.to_str().unwrap_or_default();
        debug!("host: {}", host);
        // match the host
        let result = self.get_domain(Bytes::copy_from_slice(host.as_bytes()));
        if result.is_some() {
            return Self::handle_http_request(req, result.unwrap()).await;
        }

        // match subdomain
        let subdomain = host.split('.').next().unwrap_or_default();
        let subdomain = Bytes::copy_from_slice(subdomain.as_bytes());
        let result = self.get_subdomain(subdomain);
        if result.is_none() {
            return Response::builder()
                .status(404)
                .body(BoxBody::new(Full::new(Bytes::from("tunnel not found"))))
                .unwrap();
        }

        Self::handle_http_request(req, result.unwrap()).await
    }

    pub(crate) async fn handle_http_request(
        req: Request<Incoming>,
        conn_event_sender: mpsc::Sender<event::ConnEvent>,
    ) -> Response<BoxBody<Bytes, Infallible>> {
        let connection_id = Bytes::from(Uuid::new_v4().to_string());
        let (conn_chan, mut conn_event_chan_rx) = mpsc::channel(1024);

        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();

        conn_event_sender
            .send(event::ConnEvent::Add(event::ConnWithID {
                id: connection_id.clone(),
                conn: event::Conn {
                    chan: conn_chan,
                    cancel: cancel_w,
                },
            }))
            .await
            .unwrap();

        let shutdown = CancellationToken::new();
        let shutdown_listener = shutdown.clone();
        tokio::spawn(async move {
            shutdown_listener.cancelled().await;
            conn_event_sender
                .send(event::ConnEvent::Remove(connection_id))
                .await
                .context("notify server to remove connection channel")
                .unwrap();
        });

        let data_sender = conn_event_chan_rx
            .recv()
            .await
            .with_context(|| {
                shutdown.cancel();
                "failed to receive data_sender"
            })
            .unwrap();
        let data_sender = {
            match data_sender {
                event::ConnChanDataType::DataSender(sender) => sender,
                _ => panic!(
                    "we expect to receive DataSender from data_channel_rx at the first time."
                ),
            }
        };
        let raw_req = request_to_bytes(req)
            .await
            .with_context(|| {
                shutdown.cancel();
                "failed to convert request to bytes"
            })
            .unwrap();
        // send the raw request to the tunnel
        data_sender
            .send(raw_req)
            .await
            .with_context(|| {
                shutdown.cancel();
                "failed to send raw request to the tunnel"
            })
            .unwrap();

        // response to user http request by sending data to outbound_rx
        let (outbound_tx, outbound_rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(1024);

        // read the response from the tunnel and send it back to the user
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        break;
                    }
                    Some(data) = conn_event_chan_rx.recv() => {
                        match data {
                            event::ConnChanDataType::Data(data) => {
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
            shutdown.cancel();
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

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_the_cloned_http_shares_same_registrations() {
        let http1 = Http::new(80);
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
            .send(event::ConnEvent::Remove(Bytes::from_static(
                b"example2.com",
            )))
            .await
            .is_ok());
        let received = rx1.recv().await;
        assert!(received.is_some());

        assert!(http2
            .get_subdomain(Bytes::from_static(b"foo"))
            .unwrap()
            .send(event::ConnEvent::Remove(Bytes::from_static(b"foo.com")))
            .await
            .is_ok());
        let received = rx2.recv().await;
        assert!(received.is_some());
    }
}
