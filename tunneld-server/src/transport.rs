use std::convert::Infallible;
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use dashmap::DashMap;
use http::HeaderValue;
use http_body::Frame;
use http_body_util::combinators::BoxBody;
use http_body_util::StreamBody;
use hyper::body::Body;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::io::AsyncWriteExt;
use tokio::{io, select, spawn, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tonic::Status;
use tracing::{debug, info};
use tunneld_pkg::{
    event,
    io::{StreamingReader, StreamingWriter, VecWrapper},
    util::create_listener,
};
use uuid::Uuid;

pub struct EventBus {
    vhttp_port: u16,
    _domain: String,
    http_tunnel: HttpTunnel,
}

#[derive(Clone)]
struct HttpTunnel {
    subdomains: DashMap<Bytes, mpsc::Sender<event::ConnEvent>>,
}

impl HttpTunnel {
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
}

static EMPTY_HOST: HeaderValue = HeaderValue::from_static("");

impl HttpTunnel {
    async fn call<T: Body + std::fmt::Debug>(
        &self,
        req: Request<T>,
    ) -> Response<BoxBody<Bytes, Infallible>> {
        let host = req.headers().get("host").unwrap_or(&EMPTY_HOST);
        let host = host.to_str().unwrap_or_default();
        // match the host
        debug!("host: {}", host);

        // match subdomain
        let subdomain = host.split('.').next().unwrap_or_default();
        let subdomain = Bytes::copy_from_slice(subdomain.as_bytes());
        let result = self.get_subdomain(subdomain);
        if result.is_none() {
            let chunks: Vec<Result<_, Infallible>> =
                vec![Ok(Frame::data(Bytes::from_static(b"tunnel not found")))];
            let stream = futures_util::stream::iter(chunks);
            let body = BoxBody::new(StreamBody::new(stream));
            return Response::builder().status(404).body(body).unwrap();
        }

        let conn_event_sender = result.unwrap();
        let connection_id = Uuid::new_v4().to_string();
        let (conn_chan, mut conn_event_chan_rx) = mpsc::channel(1024);

        let cancel_w = CancellationToken::new();
        let _cancel = cancel_w.clone();

        conn_event_sender
            .send(event::ConnEvent::Add(event::Conn {
                id: connection_id,
                chan: conn_chan,
                cancel: cancel_w,
            }))
            .await
            .unwrap();

        let data_sender = conn_event_chan_rx
            .recv()
            .await
            .context("failed to receive data_sender")
            .unwrap();
        let _data_sender = {
            match data_sender {
                event::ConnChanDataType::DataSender(sender) => sender,
                _ => panic!(
                    "we expect to receive DataSender from data_channel_rx at the first time."
                ),
            }
        };

        // read data from conn_event_chan_rx, write data to tunnel
        // read data from tunnel, write data to data_sender

        let chunks: Vec<Result<_, Infallible>> =
            vec![Ok(Frame::data(Bytes::from_static(b"Hello world!")))];
        let stream = futures_util::stream::iter(chunks);
        let body = BoxBody::new(StreamBody::new(stream));
        Response::builder().body(body).unwrap()
    }
}

impl EventBus {
    pub fn new(vhttp_port: u16, domain: String) -> Self {
        Self {
            vhttp_port,
            _domain: domain,
            http_tunnel: HttpTunnel {
                subdomains: DashMap::new(),
            },
        }
    }

    pub async fn listen(self, mut receiver: mpsc::Receiver<event::Event>) {
        let this = Arc::new(self);

        {
            let vhttp_listener = create_listener(this.vhttp_port).await.unwrap();
            let this = Arc::clone(&this);
            let http1_builder = Arc::new(http1::Builder::new());
            tokio::spawn(async move {
                info!("vhttp server started on port {}", this.vhttp_port);
                while let Ok((stream, _addr)) = vhttp_listener.accept().await {
                    let io = TokioIo::new(stream);
                    let this = Arc::clone(&this);
                    let http1_builder = Arc::clone(&http1_builder);

                    tokio::task::spawn(async move {
                        let new_service = service_fn(move |req| {
                            let http_tunnel = this.http_tunnel.clone();
                            async move {
                                Ok::<Response<BoxBody<Bytes, Infallible>>, hyper::Error>(
                                    http_tunnel.call(req).await,
                                )
                            }
                        });
                        let _ = http1_builder.serve_connection(io, new_service).await;
                    });
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

                    if port != 0 {
                        // we need to start a new http server on this port
                        //
                    } else if !subdomain.is_empty() {
                        // forward the http request from this subdomain to control server.
                        if this.http_tunnel.subdomain_registered(subdomain.clone()) {
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
                                this3.http_tunnel.unregister_subdomain(subdomain2);
                            });

                            this.http_tunnel
                                .register_subdomain(subdomain, event.conn_event_chan.clone());
                            event.resp.send(None).unwrap(); // success
                        }
                    } else if !domain.is_empty() {
                        // forward the http request from this domain to control server.
                        continue;
                    }
                }
            }
        }

        debug!("tcp manager quit");
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
                    let (data_channel, mut data_channel_rx) = mpsc::channel(1024);

                    let cancel_w = CancellationToken::new();
                    let cancel = cancel_w.clone();

                    let event = event::Conn{
                        id: connection_id.clone(),
                        chan: data_channel.clone(),
                        cancel: cancel_w,
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
                                    .send(event::ConnEvent::Remove(connection_id))
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
