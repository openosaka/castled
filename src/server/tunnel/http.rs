use crate::bridge::BridgeData;
use crate::event::{self, IncomingEventSender};
use crate::helper::create_tcp_listener;

use super::{init_data_sender_bridge, BridgeResult};
use anyhow::{Context as _, Result};
use bytes::{BufMut as _, Bytes};
use dashmap::DashMap;
use futures::TryStreamExt;
use http::response::Builder;
use http::{HeaderValue, StatusCode};
use http_body::Frame;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyDataStream, BodyExt, Full, StreamBody};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{body::Incoming, Request, Response};
use hyper_util::rt::TokioIo;
use std::cell::Cell;
use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::{debug, error, info, info_span, Instrument as _};

static EMPTY_HOST: HeaderValue = HeaderValue::from_static("");
const MAX_HEADERS: usize = 124;
const MAX_HEADER_SIZE: usize = 4 * 1024; // 4k

pub(crate) struct Http {
    port: u16,
    lookup: Arc<Box<dyn LookupRequest>>,
}

/// LookupRequest is a trait that provides a method to
/// lookups the request and returns [`IncomingEventSender`].
///
/// http tunnel will use [`IncomingEventSender`] to create a bridge
/// between the control server and data server when receives a request.
pub(crate) trait LookupRequest: Send + Sync {
    fn lookup(&self, req: &Request<Incoming>) -> Option<IncomingEventSender>;
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
        info!(port, "http server starting on port");
        Self { port, lookup }
    }

    pub(crate) async fn serve(self, shutdown: CancellationToken) -> Result<(), Status> {
        let vhttp_listener = tokio::select! {
            _ = shutdown.cancelled() => {
                return Ok(());
            },
            listener = create_tcp_listener(self.port) => match listener {
                Ok(listener) => listener,
                Err(err) => return Err(err),
            }
        };

        self.serve_with_listener(vhttp_listener, shutdown.clone());

        Ok(())
    }

    pub(crate) fn serve_with_listener(self, listener: TcpListener, shutdown: CancellationToken) {
        let this = Arc::new(self);
        let http1_builder = Arc::new(http1::Builder::new());
        let vhttp_handler = async move {
            loop {
                let shutdown = shutdown.clone();
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        break;
                    },
                    Ok((stream, _addr)) = listener.accept() => {
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

                                tokio::select! {
                                    _ = shutdown.cancelled() => {}
                                    _ = http1_builder.serve_connection(io, new_service) => {
                                        info!("http1 connection closed");
                                    }
                                }
                            }.instrument(info_span!("vhttp_handler"));
                            tokio::task::spawn(handler);
                        });
                    }
                }
            }
            info!(port = this.port, "http server stopped")
        }
        .instrument(info_span!("vhttp listener"));
        tokio::spawn(vhttp_handler);
    }

    async fn call(&self, req: Request<Incoming>) -> Response<BoxBody<Bytes, Infallible>> {
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
        let (headers, mut body_stream) = request_to_stream(req)
            .await
            .with_context(|| {
                bridge.remove_bridge_sender.cancel();
                "failed to convert request to bytes"
            })
            .unwrap();

        let data_sender = bridge.data_sender.clone();
        let remove_bridge_sender = bridge.remove_bridge_sender.clone();
        let client_cancel_receiver = bridge.client_cancel_receiver.clone();

        tokio::spawn(async move {
            data_sender
                .send(headers)
                .await
                .with_context(|| {
                    remove_bridge_sender.cancel();
                    "failed to send raw request to the tunnel"
                })
                .unwrap();

            loop {
                tokio::select! {
                    _ = client_cancel_receiver.cancelled() => {
                        return;
                    }
                    data = body_stream.try_next() => {
                        match data {
                            Ok(Some(data)) => {
                                data_sender.send(data.to_vec()).await.unwrap();
                            }
                            Ok(None) => {
                                // TODO(sword): reafctor this logic into io module
                                // sending a empty vec to indicate the end of the body
                                // then io::copy() will finish the transfer on the client side.
                                data_sender.send(vec![]).await.unwrap();
                                return;
                            }
                            Err(err) => {
                                error!(err = ?err, "failed to read body stream");
                                return;
                            }
                        }
                    }
                }
            }
        });

        // response to user http request by sending data to outbound_rx
        let (body_tx, body_rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(1024);
        let (header_tx, header_rx) = oneshot::channel::<Result<Builder>>();

        let client_cancel_receiver = bridge.client_cancel_receiver.clone();

        // read the response from the tunnel and send it back to the user
        tokio::spawn(receive_response(
            bridge.data_receiver,
            Some(header_tx), // wrapping Option for send once
            body_tx,
            client_cancel_receiver,
            bridge.remove_bridge_sender,
        ));

        tokio::select! {
            _ = bridge.client_cancel_receiver.cancelled() => {
                Response::builder()
                    .status(500)
                    .body(BoxBody::new(Full::new(Bytes::from_static(b"client cancelled"))))
                    .unwrap()
            }
            header = header_rx => {
                // get response builder from header
                let http_builder = header.unwrap();
                match http_builder {
                    Ok(http_builder) => {
                        let stream = ReceiverStream::new(body_rx);
                        let body = BoxBody::new(StreamBody::new(stream));
                        http_builder.body(body).unwrap()
                    },
                    Err(err) => {
                        error!(err = ?err, "failed to get response builder");
                        Response::builder()
                            .status(500)
                            .body(BoxBody::new(Full::new(Bytes::from_static(b"failed to get response builder"))))
                            .unwrap()
                    },
                }

                // let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
                // let mut resp = httparse::Response::new(&mut headers);
                // if let Err(err) = resp.parse(&header_buf) {
                //     error!(err = ?err, "failed to parse response header");
                //     return Response::builder()
                //         .status(500)
                //         .body(BoxBody::new(Full::new(Bytes::from_static(b"failed to parse response header"))))
                //         .unwrap();
                // }
                // if resp.code.is_none() {
                //     error!("invalid response header: no status");
                //     return Response::builder()
                //         .status(500)
                //         .body(BoxBody::new(Full::new(Bytes::from_static(b"invalid response header: no status"))))
                //         .unwrap();
                // }

                // let mut http_builder = Response::builder().status(resp.code.unwrap())
                //     .version(match resp.version {
                //         Some(0) => http::Version::HTTP_10,
                //         _ => http::Version::HTTP_11,
                //     });
                // for header in resp.headers {
                //     http_builder = http_builder.header(header.name, header.value);
                // }
            }
        }
    }
}

struct ResponseHeaderScanner {
    buf_cell: Cell<Vec<u8>>,
    ended: bool,
    pos: usize,
}

impl ResponseHeaderScanner {
    fn new(buf_size: usize) -> Self {
        Self {
            buf_cell: Cell::new(Vec::with_capacity(buf_size)),
            ended: false,
            pos: 0,
        }
    }

    fn split_parts(&mut self) -> (&[u8], &[u8]) {
        let buffer = self.buf_cell.get_mut();
        buffer.split_at(self.pos)
    }

    /// Helper function to find the end of the header (\r\n\r\n) in the buffer.
    /// Returns true if the end of the header if it found.
    fn scan(&mut self, new_buf: Vec<u8>) -> bool {
        let buffer = self.buf_cell.get_mut();
        buffer.extend_from_slice(&new_buf);
        for i in self.pos..buffer.len() - 3 {
            if buffer[i] == b'\r'
                && buffer[i + 1] == b'\n'
                && buffer[i + 2] == b'\r'
                && buffer[i + 3] == b'\n'
            {
                self.pos = i + 4;
                self.ended = true;
                return true;
            }
        }
        false
    }

    fn parse(&mut self) -> Result<Option<(Builder, Vec<u8>)>> {
        let (header_part, body_part) = self.split_parts();
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut resp = httparse::Response::new(&mut headers);
        resp.parse(header_part)?;
        if resp.code.is_none() {
            return Err(anyhow::anyhow!("invalid response header: no status"));
        }

        if resp.code.unwrap() == StatusCode::CONTINUE {
            // discard 100-continue, because the hyper server has handled it.
            // continue to look for the next header end.
            let mut new_header = Vec::with_capacity(MAX_HEADER_SIZE);
            new_header.extend_from_slice(body_part);
            self.reset(new_header);
            return Ok(None);
        }

        let mut http_builder =
            Response::builder()
                .status(resp.code.unwrap())
                .version(match resp.version {
                    Some(0) => http::Version::HTTP_10,
                    _ => http::Version::HTTP_11,
                });
        for header in resp.headers {
            http_builder = http_builder.header(header.name, header.value);
        }
        Ok(Some((http_builder, body_part.to_vec())))
    }

    fn move_back(&mut self, n: usize) {
        self.pos = self.pos.saturating_sub(n);
    }

    fn reset(&mut self, buffer: Vec<u8>) {
        self.ended = false;
        self.pos = 0;
        self.buf_cell.set(buffer);
    }
}

async fn receive_response(
    mut data_receiver: mpsc::Receiver<BridgeData>,
    mut header_tx: Option<oneshot::Sender<Result<Builder>>>,
    body_tx: mpsc::Sender<Result<Frame<Bytes>, Infallible>>,
    client_cancel_receiver: CancellationToken,
    remove_bridge_sender: CancellationToken,
) {
    // let mut header_cell = Cell::new(Vec::with_capacity(MAX_HEADER_SIZE));
    // let mut header_ended = false;
    // let mut scan_buf_start = 0;
    let mut header_scanner = ResponseHeaderScanner::new(MAX_HEADER_SIZE);

    loop {
        tokio::select! {
            _ = client_cancel_receiver.cancelled() => {
                break;
            }
            Some(data) = data_receiver.recv() => {
                match data {
                    BridgeData::Data(data) => {
                        if data.is_empty() {
                            if !header_scanner.ended {
                                // this spawn will drop the body_tx and header_tx
                                error!("unexpected empty data, header not ended");
                            }
                            // means no more data
                            break;
                        }

                        if !header_scanner.ended {
                            if header_scanner.scan(data) {
                                match header_scanner.parse() {
                                    Err(err) => {
                                        header_tx.take().unwrap().send(Err(err)).unwrap();
                                        break;
                                    }
                                    Ok(None) => {
                                        continue
                                    }
                                    Ok(Some((http_builder, body_part))) => {
                                        header_tx.take().unwrap().send(Ok(http_builder)).unwrap();
                                        let frame = Frame::data(Bytes::from(body_part.to_vec()));
                                        let _ = body_tx.send(Ok(frame)).await;
                                    }
                                }
                            } else {
                                header_scanner.move_back(4); // min: 0
                            }
                        } else {
                            let frame = Frame::data(Bytes::from(data));
                            let _ = body_tx.send(Ok(frame)).await;
                        }
                    }
                    _ => {
                        panic!("unexpected data type");
                    }
                }
            }
        }
    }
    remove_bridge_sender.cancel();
}

async fn request_to_stream(
    req: Request<Incoming>,
) -> Result<(Vec<u8>, StreamBody<BodyDataStream<Incoming>>)> {
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

    let body = StreamBody::new(body.into_data_stream());
    Ok((buf.into_inner(), body))
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
        debug!(host, "matching host");
        // match the host
        let result = self.get_domain(Bytes::copy_from_slice(host.as_bytes()));
        if result.is_some() {
            return result;
        }

        // match subdomain
        let subdomain = host.split('.').next().unwrap_or_default();
        debug!("matching subdomain");
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

    pub(crate) fn domain_registered(&self, domain: &Bytes) -> bool {
        self.domains.contains_key(domain)
    }

    pub(crate) fn get_domain(&self, domain: Bytes) -> Option<mpsc::Sender<event::UserIncoming>> {
        self.domains.get(&domain).map(|x| x.value().clone())
    }

    pub(crate) fn subdomain_registered(&self, subdomain: &Bytes) -> bool {
        self.subdomains.contains_key(subdomain)
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
    use std::pin::Pin;

    use futures::Future;
    use tokio::sync::oneshot;

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

        assert!(http1.domain_registered(&Bytes::from_static(b"example2.com")));
        assert!(http2.domain_registered(&Bytes::from_static(b"example1.com")));
        assert!(http1.subdomain_registered(&Bytes::from_static(b"bar")));
        assert!(http2.subdomain_registered(&Bytes::from_static(b"foo")));

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
        assert!(!http1.subdomain_registered(&Bytes::from_static(b"foo")));
        assert!(http1.subdomain_registered(&Bytes::from_static(b"bar")));

        http1.unregister_domain(Bytes::from_static(b"example2.com"));
        assert!(!http2.domain_registered(&Bytes::from_static(b"example2.com")));
        assert!(http2.domain_registered(&Bytes::from_static(b"example1.com")));
    }

    #[tokio::test]
    async fn test_receive_response() {
        struct Case<'a> {
            name: &'a str,
            send_fn: Box<
                dyn Fn(mpsc::Sender<BridgeData>) -> Pin<Box<dyn Future<Output = ()> + Send>>
                    + Send
                    + Sync
                    + 'a,
            >,
            expected_header: &'a str,
            expected_body: &'a [u8],
        }

        let cases: Vec<Case> = vec![
            Case {
                name: "response header and body in one data frame",
                send_fn: Box::new(|tx| {
                    Box::pin(async move {
                        let _ = tx
                            .send(BridgeData::Data(
                                b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello".to_vec(),
                            ))
                            .await;
                    })
                }),
                expected_header: "HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n",
                expected_body: b"hello",
            },
            Case {
                name: "response header and body in two data frames",
                send_fn: Box::new(|tx| {
                    Box::pin(async move {
                        let _ = tx
                            .send(BridgeData::Data(b"HTTP/1.1 200 OK\r\nContent-Len".to_vec()))
                            .await;
                        let _ = tx
                            .send(BridgeData::Data(b"gth: 5\r\n\r\nhello".to_vec()))
                            .await;
                    })
                }),
                expected_header: "HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n",
                expected_body: b"hello",
            },
            Case {
                name: "response header and body in two data frames, and splitted by LF",
                send_fn: Box::new(|tx| {
                    Box::pin(async move {
                        let _ = tx
                            .send(BridgeData::Data(
                                b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n".to_vec(),
                            ))
                            .await;
                        let _ = tx.send(BridgeData::Data(b"\r\nhello".to_vec())).await;
                    })
                }),
                expected_header: "HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n",
                expected_body: b"hello",
            },
            Case {
                name: "response 100 then response 200",
                send_fn: Box::new(|tx| {
                    Box::pin(async move {
                        let _ = tx
                            .send(BridgeData::Data(b"HTTP/1.1 100 Continue\r\n\r\n".to_vec()))
                            .await;
                        let _ = tx
                            .send(BridgeData::Data(
                                b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello".to_vec(),
                            ))
                            .await;
                    })
                }),
                expected_header: "HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n",
                expected_body: b"hello",
            },
        ];

        for case in cases {
            println!("running test case: {}", case.name);

            let (data_tx, data_rx) = mpsc::channel(32);
            let (header_tx, header_rx) = oneshot::channel();
            let (body_tx, mut body_rx) = mpsc::channel(32);
            let client_cancel_sender = CancellationToken::new();
            let client_cancel_receiver = client_cancel_sender.clone();
            let remove_bridge_sender = CancellationToken::new();
            let remove_bridge_receiver = remove_bridge_sender.clone();

            tokio::spawn(receive_response(
                data_rx,
                Some(header_tx),
                body_tx,
                client_cancel_receiver,
                remove_bridge_sender,
            ));

            (case.send_fn)(data_tx).await;
            let http_builder = header_rx.await.unwrap().unwrap();
            assert_eq!(http_builder_to_string(http_builder), case.expected_header);
            let body = body_rx.recv().await.unwrap().unwrap();
            assert_eq!(
                body.into_data().unwrap(),
                Bytes::from_static(case.expected_body)
            );

            client_cancel_sender.cancel();
            remove_bridge_receiver.cancelled().await;
        }
    }

    fn http_builder_to_string<'a>(builder: Builder) -> &'a str {
        let (parts, _) = builder.body(()).unwrap().into_parts();
        let mut buf = String::new();
        buf.push_str(&format!(
            "{:?} {:?} {}\r\n",
            parts.version,
            parts.status,
            parts.status.canonical_reason().unwrap(),
        ));
        for (key, value) in parts.headers.iter() {
            buf.push_str(&format!("{}: {}\r\n", key, value.to_str().unwrap()));
        }
        buf.push_str("\r\n");
        Box::leak(buf.into_boxed_str())
    }
}
