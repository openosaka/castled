use std::{
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Future;
use tokio_util::sync::CancellationToken;

#[macro_export]
macro_rules! get_with_shutdown {
    ($do:expr, $shutdown:expr) => {{
        tokio::select! {
            result = $do => {
                result
            }
            _ = $shutdown => {
                return Ok(());
            }
        }
    }};
}

pub struct Shutdown {
    inner: CancellationToken,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    pub fn new() -> Self {
        Self {
            inner: CancellationToken::new(),
        }
    }

    pub fn listen(&self) -> ShutdownListener {
        ShutdownListener {
            notify: self.inner.clone(),
        }
    }

    pub fn notify(&self) {
        self.inner.cancel();
    }
}

impl Deref for Shutdown {
    type Target = CancellationToken;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct ShutdownListener {
    notify: CancellationToken,
}

impl ShutdownListener {
    pub async fn done(&self) {
        self.notify.cancelled().await;
    }

    pub fn from_cancellation(cancel: CancellationToken) -> Self {
        Self { notify: cancel }
    }
}

impl Deref for ShutdownListener {
    type Target = CancellationToken;

    fn deref(&self) -> &Self::Target {
        &self.notify
    }
}

impl Clone for ShutdownListener {
    fn clone(&self) -> Self {
        Self {
            notify: self.notify.clone(),
        }
    }
}

impl Future for ShutdownListener {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.notify.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_shutdown() {
        let shutdown = Shutdown::new();
        // create n listeners by pipeline
        let listeners = (0..100).map(|_| shutdown.listen()).collect::<Vec<_>>();

        tokio::spawn(async move {
            sleep(Duration::from_secs(1)).await;
            shutdown.notify();
        });

        let mut cancelled = 0;
        for listener in listeners {
            listener.done().await;
            cancelled += 1;
        }
        assert_eq!(cancelled, 100);
    }

    #[tokio::test]
    async fn test_with_shutdown() {
        let shutdown = Shutdown::new();
        let shutdown_listener = shutdown.listen();

        tokio::spawn(async move {
            sleep(Duration::from_micros(1000)).await;
            shutdown.notify();
        });

        let result: Result<(), Error> = async {
            get_with_shutdown!(
                async {
                    sleep(Duration::from_secs(10)).await;
                    Ok(())
                },
                shutdown_listener.cancelled()
            )
        }
        .await;

        assert!(result.is_ok());
    }
}
