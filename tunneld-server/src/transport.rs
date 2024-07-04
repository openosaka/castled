use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use bytes::{BufMut, Bytes};
use dashmap::DashMap;
use http::HeaderValue;
use http_body::Frame;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::Sender;
use tokio::{io, select, spawn, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::{debug, info};
use tunneld_pkg::get_with_shutdown;
use tunneld_pkg::shutdown::ShutdownListener;
use tunneld_pkg::{
    event,
    io::{StreamingReader, StreamingWriter, VecWrapper},
    util::create_listener,
};
use uuid::Uuid;

static EMPTY_HOST: HeaderValue = HeaderValue::from_static("");

pub struct EventBus {
    vhttp_port: u16,
    _domain: String,
    // tunneld provides a vhttp server for responding requests to the tunnel
    // which is used different subdomains or domains, they still use the same port.
    vhttp_tunnel: HttpTunnel,
}

#[derive(Clone)]
struct HttpTunnel {
    domains: DashMap<Bytes, mpsc::Sender<event::ConnEvent>>,
    subdomains: DashMap<Bytes, mpsc::Sender<event::ConnEvent>>,
}

impl HttpTunnel {
    fn register_domain(&self, domain: Bytes, conn_event_chan: mpsc::Sender<event::ConnEvent>) {
        self.domains.insert(domain, conn_event_chan);
    }

    fn unregister_domain(&self, domain: Bytes) {
        self.domains.remove(&domain);
    }

    fn domain_registered(&self, domain: Bytes) -> bool {
        self.domains.contains_key(&domain)
    }

    fn get_domain(&self, domain: Bytes) -> Option<mpsc::Sender<event::ConnEvent>> {
        self.domains.get(&domain).map(|x| x.value().clone())
    }

    fn subdomain_registered(&self, subdomain: Bytes) -> bool {
        self.subdomains.contains_key(&subdomain)
    }

    fn unregister_subdomain(&self, subdomain: Bytes) {
        self.subdomains.remove(&subdomain);
    }

    fn get_subdomain(&self, subdomain: Bytes) -> Option<mpsc::Sender<event::ConnEvent>> {
        self.subdomains.get(&subdomain).map(|x| x.value().clone())
    }

    fn register_subdomain(
        &self,
        subdomain: Bytes,
        conn_event_chan: mpsc::Sender<event::ConnEvent>,
    ) {
        self.subdomains.insert(subdomain, conn_event_chan);
    }

    async fn call(&self, req: Request<Incoming>) -> Response<BoxBody<Bytes, Infallible>> {
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

    async fn handle_http_request(
        req: Request<Incoming>,
        conn_event_sender: Sender<event::ConnEvent>,
    ) -> Response<BoxBody<Bytes, Infallible>> {
        let connection_id = Uuid::new_v4().to_string();
        let (conn_chan, mut conn_event_chan_rx) = mpsc::channel(1024);

        let cancel_w = CancellationToken::new();
        let cancel = cancel_w.clone();

        conn_event_sender
            .send(event::ConnEvent::Add(event::ConnWithID {
                id: Bytes::from(connection_id),
                conn: event::Conn {
                    chan: conn_chan,
                    cancel: cancel_w,
                },
            }))
            .await
            .unwrap();

        let data_sender = conn_event_chan_rx
            .recv()
            .await
            .context("failed to receive data_sender")
            .unwrap();
        let data_sender = {
            match data_sender {
                event::ConnChanDataType::DataSender(sender) => sender,
                _ => panic!(
                    "we expect to receive DataSender from data_channel_rx at the first time."
                ),
            }
        };
        let raw_req = request_to_bytes(req).await.unwrap();
        // send the raw request to the tunnel
        data_sender.send(raw_req).await.unwrap();

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

impl EventBus {
    pub fn new(vhttp_port: u16, domain: String) -> Self {
        Self {
            vhttp_port,
            _domain: domain,
            vhttp_tunnel: HttpTunnel {
                domains: DashMap::new(),
                subdomains: DashMap::new(),
            },
        }
    }

    pub async fn listen(
        self,
        shutdown: ShutdownListener,
        mut receiver: mpsc::Receiver<event::Event>,
    ) -> anyhow::Result<()> {
        let this = Arc::new(self);
        let this2 = Arc::clone(&this);

        let http1_builder = Arc::new(http1::Builder::new());
        {
            let vhttp_listener =
                get_with_shutdown!(create_listener(this.vhttp_port), shutdown.clone())?;

            let http1_builder = Arc::clone(&http1_builder);
            tokio::spawn(async move {
                info!("vhttp server started on port {}", this2.vhttp_port);
                loop {
                    tokio::select! {
                        _ = shutdown.clone() => {},
                        Ok((stream, _addr)) = vhttp_listener.accept() => {
                            let io = TokioIo::new(stream);
                            let this = Arc::clone(&this2);
                            let http1_builder = Arc::clone(&http1_builder);

                            tokio::task::spawn(async move {
                                let new_service = service_fn(move |req| {
                                    let http_tunnel = this.vhttp_tunnel.clone();
                                    async move {
                                        Ok::<Response<BoxBody<Bytes, Infallible>>, hyper::Error>(
                                            http_tunnel.call(req).await,
                                        )
                                    }
                                });
                                http1_builder.serve_connection(io, new_service).await
                            });
                        }
                    }
                }
            });
        }

        while let Some(event) = receiver.recv().await {
            match event.payload {
                event::Payload::RegisterTcp { port } => match create_listener(port).await {
                    Ok(listener) => {
                        let cancel = event.cancel;
                        let conn_event_chan = event.conn_event_chan;
                        let this2 = Arc::clone(&this);
                        spawn(async move {
                            this2
                                .handle_tcp_listener(listener, cancel, conn_event_chan.clone())
                                .await;
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
                    if port == 0 && subdomain.is_empty() && domain.is_empty() {
                        event
                            .resp
                            .send(Some(Status::invalid_argument(
                                "invalid http tunnel arguments",
                            )))
                            .unwrap();
                        continue;
                    }

                    if !subdomain.is_empty() {
                        // forward the http request from this subdomain to control server.
                        if this.vhttp_tunnel.subdomain_registered(subdomain.clone()) {
                            event
                                .resp
                                .send(Some(Status::already_exists("subdomain already registered")))
                                .unwrap();
                        } else {
                            let subdomain2 = subdomain.clone();

                            // unregister the vhost when client disconnects.
                            let this3 = Arc::clone(&this);
                            tokio::spawn(async move {
                                event.cancel.cancelled().await;
                                this3.vhttp_tunnel.unregister_subdomain(subdomain2);
                            });

                            this.vhttp_tunnel
                                .register_subdomain(subdomain, event.conn_event_chan.clone());
                            event.resp.send(None).unwrap(); // success
                        }
                    } else if !domain.is_empty() {
                        // forward the http request from this domain to control server.
                        if this.vhttp_tunnel.domain_registered(domain.clone()) {
                            event
                                .resp
                                .send(Some(Status::already_exists("domain already registered")))
                                .unwrap();
                        } else {
                            let domain2 = domain.clone();

                            // unregister the vhost when client disconnects.
                            let this4 = Arc::clone(&this);
                            tokio::spawn(async move {
                                event.cancel.cancelled().await;
                                this4.vhttp_tunnel.unregister_domain(domain2);
                            });

                            this.vhttp_tunnel
                                .register_domain(domain, event.conn_event_chan.clone());
                            event.resp.send(None).unwrap(); // success
                        }
                    } else if port != 0 {
                        let http1_builder = Arc::clone(&http1_builder);

                        let listener = create_listener(port).await;
                        if listener.is_err() {
                            if let Err(err) = event.resp.send(Some(listener.unwrap_err())) {
                                debug!("failed to send response to register event: {:?}", err);
                            }
                            continue; // just continue to the next event
                        }
                        event.resp.send(None).unwrap(); // success
                        let listener = listener.unwrap();

                        tokio::spawn(async move {
                            info!("listen http on {}", port);

                            let conn_event_chan = event.conn_event_chan;

                            loop {
                                tokio::select! {
                                    _ = event.cancel.cancelled() => {
                                        info!("closing http listener on {}", port);
                                        return;
                                    }
                                    Ok((stream, _)) = listener.accept() => {
                                        let http1_builder = Arc::clone(&http1_builder);
                                        let conn_event_chan = conn_event_chan.clone();
                                            let io = TokioIo::new(stream);

                                            tokio::task::spawn(async move {
                                                let conn_event_chan = conn_event_chan.clone();
                                                let new_service = service_fn(move |req| {
                                                    let conn_event_chan = conn_event_chan.clone();
                                                    async move {
                                                        Ok::<
                                                            Response<BoxBody<Bytes, Infallible>>,
                                                            hyper::Error,
                                                        >(
                                                            HttpTunnel::handle_http_request(
                                                                req,
                                                                conn_event_chan,
                                                            )
                                                            .await,
                                                        )
                                                    }
                                                });
                                                http1_builder.serve_connection(io, new_service).await
                                            });
                                    }
                                }
                            }
                        });
                    } else {
                        unreachable!("maybe give it a random port or subdomain, I don't have this requirement yet.");
                    }
                }
            }
        }

        debug!("tcp manager quit");
        Ok(())
    }

    async fn handle_tcp_listener(
        &self,
        listener: tokio::net::TcpListener,
        shutdown: CancellationToken,
        conn_event_chan: mpsc::Sender<event::ConnEvent>,
    ) {
        loop {
            select! {
                _ = shutdown.cancelled() => {
                    return;
                }
                result = async {
                    match listener.accept().await  {
                        Ok(result) => {
                            Some(result)
                        }
                        Err(err) => {
                            debug!("failed to accept connection: {:?}", err);
                            None
                        }
                    }
                } => {
                    if result.is_none() {
                        return;
                    }
                    let (stream, _addr) = result.unwrap();
                    let connection_id = Uuid::new_v4().to_string();
                    let connection_id_bytes = Bytes::from(connection_id.clone());
                    let (data_channel, mut data_channel_rx) = mpsc::channel(1024);

                    let cancel_w = CancellationToken::new();
                    let cancel = cancel_w.clone();

                    let event = event::ConnWithID{
                        id: connection_id_bytes.clone(),
                        conn: event::Conn{
                            chan: data_channel.clone(),
                            cancel: cancel_w,
                        }
                    };
                    conn_event_chan.send(event::ConnEvent::Add(event)).await.unwrap();
                    let data_sender = data_channel_rx.recv().await.context("failed to receive data_sender").unwrap();
                    let data_sender = {
                        match data_sender {
                            event::ConnChanDataType::DataSender(sender) => sender,
                            _ => panic!("we expect to receive DataSender from data_channel_rx at the first time."),
                        }
                    };

                    let conn_event_chan_for_removing = conn_event_chan.clone();
                    tokio::spawn(async move {
                        let (mut remote_reader, mut remote_writer) = stream.into_split();
                        let wrapper = VecWrapper::<Vec<u8>>::new();
                        let mut tunnel_writer = StreamingWriter::new(data_sender, wrapper);
                        let mut tunnel_reader = StreamingReader::new(data_channel_rx); // we expect to receive data from data_channel_rx after the first time.
                        let remote_to_me_to_tunnel = async {
                            io::copy(&mut remote_reader, &mut tunnel_writer).await.unwrap();
                            tunnel_writer.shutdown().await.context("failed to shutdown tunnel writer").unwrap();
                            debug!("finished the transfer between remote and tunnel");
                        };
                        let tunnel_to_me_to_remote = async {
                            io::copy(&mut tunnel_reader, &mut remote_writer).await.unwrap();
                            remote_writer.shutdown().await.context("failed to shutdown remote writer").unwrap();
                            debug!("finished the transfer between tunnel and remote");
                        };

                        tokio::select! {
                            _ = async { tokio::join!(remote_to_me_to_tunnel, tunnel_to_me_to_remote) } => {
                                debug!("closing user connection {}", connection_id);
                                conn_event_chan_for_removing
                                    .send(event::ConnEvent::Remove(connection_id_bytes))
                                    .await
                                    .context("notify server to remove connection channel")
                                    .unwrap();
                            }
                            _ = cancel.cancelled() => {
                                let _ = remote_writer.shutdown().await;
                                let _ = tunnel_writer.shutdown().await;
                            }
                        }
                    });
                }
            }
        }
    }
}
