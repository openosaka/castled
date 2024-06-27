use std::task::Poll;

use futures::ready;
use tokio::{
    io,
    sync::mpsc::{self, Sender},
};
use tokio_util::sync::PollSender;
use tracing::debug;
use tunneld_protocol::pb::{traffic_to_server, TrafficToClient, TrafficToServer};

use crate::event::ConnectionChannelDataType;

pub struct StreamingReader<T> {
    receiver: mpsc::Receiver<T>,
}

impl<T: Send> StreamingReader<T> {
    pub fn new(receiver: mpsc::Receiver<T>) -> Self {
        Self { receiver }
    }
}

/// read data from TrafficToClient
impl tokio::io::AsyncRead for StreamingReader<TrafficToClient> {
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

impl tokio::io::AsyncRead for StreamingReader<ConnectionChannelDataType> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match ready!(self.receiver.poll_recv(cx)) {
            Some(data) => {
                match data {
                    ConnectionChannelDataType::DataSender(_) => {
                        panic!("we should not receive DataSender")
                    }
                    ConnectionChannelDataType::Data(data) => {
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

impl<T: Send> StreamingWriter<T> {
    pub fn new(sender: Sender<T>, wrapper: Box<dyn WriteDataWrapper<T>>) -> Self {
        Self {
            sender: PollSender::new(sender),
            wrapper,
        }
    }
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

macro_rules! generate_async_write_impl {
    ($type:ty) => {
        impl tokio::io::AsyncWrite for StreamingWriter<$type> {
            fn poll_write(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &[u8],
            ) -> Poll<std::result::Result<usize, std::io::Error>> {
                debug!("writing {} bytes to streaming", buf.len());

                // if the buffer is empty, return Ok(0) means the write operation is done
                if buf.len() == 0 {
                    return Poll::Ready(Ok(0));
                }

                match ready!(self.sender.poll_reserve(cx)) {
                    Ok(_) => {
                        let buf = self.wrapper.wrap_write(buf);
                        if let Err(err) = self.sender.send_item(buf) {
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

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> Poll<std::result::Result<(), std::io::Error>> {
                debug!("flushing streaming writer");

                // our writer writes data to the buf synchronously, so we don't need to flush
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> Poll<std::result::Result<(), std::io::Error>> {
                debug!("shutting down streaming writer");

                match ready!(self.sender.poll_reserve(cx)) {
                    Ok(_) => {
                        let buf = self.wrapper.wrap_shutdown();
                        if let Err(err) = self.sender.send_item(buf) {
                            debug!("failed to send data: {:?}", err);
                            return Poll::Ready(Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "failed to send data",
                            )));
                        }
                    }
                    Err(e) => {
                        return Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("failed to shutdown: {:?}", e),
                        )));
                    }
                }

                Poll::Ready(Ok(()))
            }
        }
    };
}

generate_async_write_impl!(TrafficToServer);
generate_async_write_impl!(Vec<u8>);
