use crate::bridge::BridgeData;
use crate::pb::traffic_to_server;
use crate::pb::TrafficToClient;
use crate::pb::TrafficToServer;
use futures::ready;
use futures::Stream;
use std::fmt::Debug;
use std::task::{Context, Poll};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::{io, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tokio_util::sync::PollSender;
use tracing::debug;

/// A wrapper around mpsc::Receiver that cancels the CancellationToken when dropped.
pub struct CancellableReceiver<T> {
    cancel: CancellationToken,
    inner: mpsc::Receiver<T>,
}

impl<T> CancellableReceiver<T> {
    pub fn new(cancel: CancellationToken, inner: mpsc::Receiver<T>) -> Self {
        Self { cancel, inner }
    }
}

impl<T> Stream for CancellableReceiver<T> {
    type Item = T;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.inner.poll_recv(cx)
    }
}

impl<T> std::ops::Deref for CancellableReceiver<T> {
    type Target = mpsc::Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> Drop for CancellableReceiver<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub struct StreamingReader<T> {
    receiver: mpsc::Receiver<T>,
}

impl<T: Send> StreamingReader<T> {
    pub fn new(receiver: mpsc::Receiver<T>) -> Self {
        Self { receiver }
    }
}

/// read data from TrafficToClient
impl AsyncRead for StreamingReader<TrafficToClient> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match ready!(self.receiver.poll_recv(cx)) {
            Some(traffic) => {
                buf.put_slice(traffic.data.as_slice());
                Poll::Ready(Ok(()))
            }
            None => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncRead for StreamingReader<BridgeData> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match ready!(self.receiver.poll_recv(cx)) {
            Some(data) => {
                match data {
                    BridgeData::Sender(_) => {
                        panic!("we should not receive DataSender")
                    }
                    BridgeData::Data(data) => {
                        buf.put_slice(data.as_slice());
                    }
                }
                Poll::Ready(Ok(()))
            }
            None => Poll::Ready(Ok(())),
        }
    }
}

pub struct StreamingWriter<T> {
    sender: PollSender<T>,
    wrapper: Box<dyn WriteDataWrapper<T>>,
}

pub trait WriteDataWrapper<T>: Send {
    fn wrap_write(&self, buf: &[u8]) -> T;
    fn wrap_shutdown(&self) -> T;
}

pub struct TrafficToServerWrapper {
    connection_id: String,
}

impl TrafficToServerWrapper {
    pub fn new(connection_id: String) -> Box<Self> {
        Box::new(Self { connection_id })
    }
}

pub struct VecWrapper<T> {
    _phantom: std::marker::PhantomData<T>,
}

impl VecWrapper<Vec<u8>> {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            _phantom: std::marker::PhantomData,
        })
    }
}

impl WriteDataWrapper<Vec<u8>> for VecWrapper<Vec<u8>> {
    fn wrap_write(&self, buf: &[u8]) -> Vec<u8> {
        buf.to_vec()
    }

    fn wrap_shutdown(&self) -> Vec<u8> {
        Vec::new()
    }
}

impl WriteDataWrapper<TrafficToServer> for TrafficToServerWrapper {
    fn wrap_write(&self, buf: &[u8]) -> TrafficToServer {
        TrafficToServer {
            connection_id: self.connection_id.clone(),
            action: traffic_to_server::Action::Sending as i32,
            data: buf.to_vec(),
        }
    }

    fn wrap_shutdown(&self) -> TrafficToServer {
        TrafficToServer {
            connection_id: self.connection_id.clone(),
            action: traffic_to_server::Action::Finished as i32,
            ..Default::default()
        }
    }
}

impl<T: Send + Debug> StreamingWriter<T> {
    pub fn new(sender: Sender<T>, wrapper: Box<dyn WriteDataWrapper<T>>) -> Self {
        Self {
            sender: PollSender::new(sender),
            wrapper,
        }
    }

    fn poll_write_impl(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>> {
        debug!("writing {} bytes to streaming", buf.len());

        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        match ready!(self.sender.poll_reserve(cx)) {
            Ok(_) => {
                let wrapped_buf = self.wrapper.wrap_write(buf);
                if let Err(err) = self.sender.send_item(wrapped_buf) {
                    debug!("failed to send data: {:?}", err);
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "failed to send data",
                    )));
                }
            }
            Err(e) => {
                debug!("failed to send data: {:?}", e);
            }
        }

        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush_impl(
        &self,
        _cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        debug!("flushing streaming");
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown_impl(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        debug!("shutting down streaming writer");

        match ready!(self.sender.poll_reserve(cx)) {
            Ok(_) => {
                let shutdown_buf = self.wrapper.wrap_shutdown();
                if let Err(err) = self.sender.send_item(shutdown_buf) {
                    debug!("failed to send data: {:?}", err);
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "failed to send data",
                    )));
                }
            }
            Err(err) => {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("failed to shutdown: {:?}", err),
                )));
            }
        }

        Poll::Ready(Ok(()))
    }
}

macro_rules! generate_async_write_impl {
    ($type:ty) => {
        impl AsyncWrite for StreamingWriter<$type> {
            fn poll_write(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<std::result::Result<usize, std::io::Error>> {
                self.poll_write_impl(cx, buf)
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::result::Result<(), std::io::Error>> {
                self.poll_flush_impl(cx)
            }

            fn poll_shutdown(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::result::Result<(), std::io::Error>> {
                self.poll_shutdown_impl(cx)
            }
        }
    };
}

generate_async_write_impl!(TrafficToServer);
generate_async_write_impl!(Vec<u8>);

pub(crate) struct AsyncUdpSocket<'a> {
    socket: &'a UdpSocket,
}

impl<'a> AsyncUdpSocket<'a> {
    pub(crate) fn new(socket: &'a UdpSocket) -> Self {
        Self { socket }
    }
}

impl<'a> AsyncRead for AsyncUdpSocket<'a> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut().socket.poll_recv_from(cx, buf) {
            Poll::Ready(Ok(_addr)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<'a> AsyncWrite for AsyncUdpSocket<'a> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>> {
        self.get_mut().socket.poll_send(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        Poll::Ready(Ok(())) // No-op for UDP
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        Poll::Ready(Ok(())) // No-op for UDP
    }
}
