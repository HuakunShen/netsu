//! Transport-agnostic ordered byte stream. Control channels always speak
//! this trait; concrete transports (TCP, UDP, WebSocket) implement it above
//! this module. [`MemoryPipe`] is the in-memory double every unit test in
//! this crate builds on instead of real sockets.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};

use crate::error::{NetsuError, Result};

/// Transport-agnostic ordered byte stream.
pub trait BytePipe: Send {
    /// Resolves with exactly `n` bytes; errors on EOF/close/timeout.
    fn read_exact(
        &mut self,
        n: usize,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send;

    /// Resolves once the bytes are handed to the transport (backpressure point).
    fn write_all(&mut self, data: &[u8]) -> impl Future<Output = Result<()>> + Send;

    fn close(&mut self) -> impl Future<Output = ()> + Send;
}

/// Shared buffering logic: a transport feeds bytes in, `read_exact` pulls
/// them back out, waiting for more if not enough have arrived yet.
struct Buffer {
    inner: Mutex<Inner>,
    notify: Notify,
}

struct Inner {
    data: VecDeque<u8>,
    closed: bool,
}

impl Buffer {
    fn new() -> Arc<Self> {
        Arc::new(Buffer {
            inner: Mutex::new(Inner {
                data: VecDeque::new(),
                closed: false,
            }),
            notify: Notify::new(),
        })
    }

    async fn feed(&self, data: &[u8]) {
        self.inner.lock().await.data.extend(data.iter().copied());
        self.notify.notify_waiters();
    }

    async fn end(&self) {
        self.inner.lock().await.closed = true;
        self.notify.notify_waiters();
    }

    async fn read_exact(&self, n: usize) -> Result<Vec<u8>> {
        loop {
            // Register interest *before* checking the condition so a feed()/end()
            // that races in after we drop the lock is not missed.
            let notified = self.notify.notified();
            {
                let mut inner = self.inner.lock().await;
                if inner.data.len() >= n {
                    return Ok(inner.data.drain(..n).collect());
                }
                if inner.closed {
                    return Err(NetsuError::PipeClosed);
                }
            }
            notified.await;
        }
    }
}

/// In-memory pipe pair for unit tests.
pub struct MemoryPipe {
    read_buf: Arc<Buffer>,
    write_target: Arc<Buffer>,
}

impl MemoryPipe {
    /// Creates a connected pair: bytes written to one side arrive on the
    /// other, in order.
    pub fn pair() -> (MemoryPipe, MemoryPipe) {
        let inbox_a = Buffer::new();
        let inbox_b = Buffer::new();
        let a = MemoryPipe {
            read_buf: inbox_a.clone(),
            write_target: inbox_b.clone(),
        };
        let b = MemoryPipe {
            read_buf: inbox_b,
            write_target: inbox_a,
        };
        (a, b)
    }
}

impl BytePipe for MemoryPipe {
    fn read_exact(
        &mut self,
        n: usize,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send {
        let buf = self.read_buf.clone();
        async move {
            match timeout {
                Some(d) => tokio::time::timeout(d, buf.read_exact(n))
                    .await
                    .map_err(|_| NetsuError::Timeout)?,
                None => buf.read_exact(n).await,
            }
        }
    }

    fn write_all(&mut self, data: &[u8]) -> impl Future<Output = Result<()>> + Send {
        let buf = self.write_target.clone();
        let data = data.to_vec();
        async move {
            buf.feed(&data).await;
            Ok(())
        }
    }

    fn close(&mut self) -> impl Future<Output = ()> + Send {
        let read_buf = self.read_buf.clone();
        let write_target = self.write_target.clone();
        async move {
            read_buf.end().await;
            write_target.end().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delivers_written_bytes_respecting_chunk_boundaries() {
        let (mut a, mut b) = MemoryPipe::pair();
        a.write_all(&[1, 2, 3, 4, 5]).await.unwrap();
        assert_eq!(b.read_exact(2, None).await.unwrap(), vec![1, 2]);
        assert_eq!(b.read_exact(3, None).await.unwrap(), vec![3, 4, 5]);
    }

    #[tokio::test]
    async fn read_exact_waits_for_enough_bytes() {
        let (mut a, mut b) = MemoryPipe::pair();
        let task = tokio::spawn(async move { b.read_exact(4, None).await });
        a.write_all(&[9]).await.unwrap();
        a.write_all(&[8, 7, 6]).await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), vec![9, 8, 7, 6]);
    }

    #[tokio::test]
    async fn read_exact_errors_on_close() {
        let (mut a, mut b) = MemoryPipe::pair();
        let task = tokio::spawn(async move { b.read_exact(1, None).await });
        a.close().await;
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn read_exact_honors_its_timeout() {
        let (_a, mut b) = MemoryPipe::pair();
        let got = b.read_exact(1, Some(Duration::from_millis(50))).await;
        assert!(matches!(got, Err(crate::error::NetsuError::Timeout)));
    }
}
