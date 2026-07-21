//! TCP transport: one socket, two lives.
//!
//! [`TcpPipe`] is the control channel's `BytePipe` — pull-based, used for the
//! cookie/state/JSON handshake. Once the handshake completes,
//! [`TcpPipe::into_data_channel`] hands the same socket to [`TcpDataChannel`]
//! for the bulk test payload, which speaks the (different) `DataChannel`
//! trait instead.
//!
//! Compare with `packages/netsu/src/transport/tcp.ts`: that implementation
//! carries four runtime guards, each documenting a review-found bug.
//! `into_data_channel(self)` here *consumes* the pipe, so two of them —
//! "write after detach" and "close after detach" — are unrepresentable: the
//! caller no longer has a `TcpPipe` to call those methods on, so the type
//! system enforces what the TS version had to check at runtime. The third
//! guard (refusing to detach with bytes still buffered) is kept below,
//! because the protocol's guarantee that no bytes are buffered at that point
//! is exactly that — a guarantee, not a proof the compiler can check, and
//! violating it would silently corrupt the data-channel bytestream. The
//! fourth — the TS connect-timeout fix — is `CONNECT_TIMEOUT` /
//! `TcpPipe::connect`'s `tokio::time::timeout` wrap below.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{NetsuError, Result};
use crate::protocol::pipe::BytePipe;
use crate::streams::channel::DataChannel;

/// Default connect deadline. The TypeScript implementation shipped without
/// one and an unreachable host hung the connect promise forever; flagged in
/// review and fixed there with `socket.setTimeout`. Here `TcpPipe::connect`
/// wraps the whole resolve-and-connect in `tokio::time::timeout`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Control-channel view of a `TcpStream`: buffers inbound bytes so
/// `read_exact` can pull out exactly the amount the caller asks for,
/// regardless of how the underlying reads happened to chunk.
pub struct TcpPipe {
    stream: TcpStream,
    buf: VecDeque<u8>,
}

impl TcpPipe {
    /// Wraps an already-connected stream, e.g. from `TcpListener::accept`.
    pub fn from_stream(stream: TcpStream) -> Self {
        // Best-effort: control-channel framing is small and latency-sensitive
        // (single state bytes, short JSON), so Nagle's algorithm would add
        // needless delay. Failure here isn't actionable and isn't fatal.
        let _ = stream.set_nodelay(true);
        TcpPipe {
            stream,
            buf: VecDeque::new(),
        }
    }

    /// Connects to `host:port`, honoring `timeout` for both DNS resolution
    /// and the TCP handshake — an unreachable host errors out rather than
    /// hanging forever.
    pub async fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpPipe> {
        let stream = tokio::time::timeout(timeout, TcpStream::connect((host, port)))
            .await
            .map_err(|_| NetsuError::Timeout)??;
        stream.set_nodelay(true)?;
        Ok(TcpPipe {
            stream,
            buf: VecDeque::new(),
        })
    }

    /// Hands the socket to a [`TcpDataChannel`] for the bulk payload.
    ///
    /// Errors if bytes are still buffered: the protocol guarantees the
    /// handshake leaves none, and silently discarding them here would drop
    /// the start of the data-channel bytestream instead of surfacing the
    /// violated assumption.
    pub fn into_data_channel(self) -> Result<TcpDataChannel> {
        if !self.buf.is_empty() {
            return Err(NetsuError::Protocol(format!(
                "into_data_channel: {} buffered byte(s) would be lost",
                self.buf.len()
            )));
        }
        Ok(TcpDataChannel::new(self.stream))
    }

    // Inherent forwarders so callers can use `TcpPipe` without importing the
    // `BytePipe` trait — the trait stays the abstraction transports are
    // written against generically (see `protocol::framing`'s `read_json` /
    // `write_json`), while a concrete `TcpPipe` remains usable on its own.
    // Fully-qualified syntax below picks the trait impl explicitly; it is
    // not a recursive call to these very methods.

    pub async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        <Self as BytePipe>::read_exact(self, n, timeout).await
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        <Self as BytePipe>::write_all(self, data).await
    }

    pub async fn close(&mut self) {
        <Self as BytePipe>::close(self).await
    }
}

impl BytePipe for TcpPipe {
    async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let fill = async {
            while self.buf.len() < n {
                let mut chunk = [0u8; 8192];
                let read = self.stream.read(&mut chunk).await?;
                if read == 0 {
                    return Err(NetsuError::PipeClosed);
                }
                self.buf.extend(chunk[..read].iter().copied());
            }
            Ok(self.buf.drain(..n).collect())
        };
        match timeout {
            Some(d) => tokio::time::timeout(d, fill)
                .await
                .map_err(|_| NetsuError::Timeout)?,
            None => fill.await,
        }
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        // tokio's AsyncWriteExt::write_all does not return until the kernel
        // has accepted every byte, so this call *is* the backpressure point
        // — no drain-equivalent needed, unlike the Socket.write() callback
        // dance the TS implementation needed (and got wrong twice; see
        // tcp.ts's TcpDataChannel guard comments).
        self.stream.write_all(data).await.map_err(NetsuError::from)
    }

    async fn close(&mut self) {
        let _ = self.stream.shutdown().await;
    }
}

/// Bulk payload channel over a detached `TcpStream`.
pub struct TcpDataChannel {
    stream: TcpStream,
    /// Failure latched so `error()` still reports it during result
    /// finalization even if the caller's last observed call succeeded, or if
    /// they never called this channel again after the failing one. Rust's
    /// `Result` already surfaces a failure to whichever call triggered it —
    /// this exists for the case where that specific `Result` isn't the thing
    /// consulted at teardown time (e.g. a read loop that only branches on
    /// `Ok(0) | Err(_)`, discarding the error itself).
    error: Option<NetsuError>,
}

impl TcpDataChannel {
    fn new(stream: TcpStream) -> Self {
        TcpDataChannel {
            stream,
            error: None,
        }
    }

    /// Records `err`'s message without needing `NetsuError: Clone` (it isn't
    /// — it wraps `std::io::Error`, which isn't `Clone` either). Keeps the
    /// first failure: once poisoned, later errors are almost always the same
    /// underlying cause (writes/reads on an already-broken socket).
    fn latch(&mut self, err: &NetsuError) {
        self.error
            .get_or_insert_with(|| NetsuError::Protocol(err.to_string()));
    }

    // Inherent forwarders, for the same reason `TcpPipe` has them: a caller
    // holding a concrete `TcpDataChannel` shouldn't need to import
    // `DataChannel` just to call its methods by name.

    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        <Self as DataChannel>::write_chunk(self, chunk).await
    }

    pub async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        <Self as DataChannel>::read_chunk(self, buf).await
    }

    pub async fn close(&mut self) {
        <Self as DataChannel>::close(self).await
    }
}

#[async_trait]
impl DataChannel for TcpDataChannel {
    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        if let Some(err) = &self.error {
            return Err(NetsuError::Protocol(err.to_string()));
        }
        match self.stream.write_all(chunk).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let err = NetsuError::from(e);
                self.latch(&err);
                Err(err)
            }
        }
    }

    async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        if let Some(err) = &self.error {
            return Err(NetsuError::Protocol(err.to_string()));
        }
        match self.stream.read(buf).await {
            Ok(n) => Ok(n),
            Err(e) => {
                let err = NetsuError::from(e);
                self.latch(&err);
                Err(err)
            }
        }
    }

    async fn close(&mut self) {
        // Bare shutdown: any bytes already in the kernel receive queue that
        // we haven't read yet are simply left unread, not an error — the
        // control channel has already told both sides the test is over by
        // the time close() is called.
        let _ = self.stream.shutdown().await;
    }

    fn error(&self) -> Option<&NetsuError> {
        self.error.as_ref()
    }
}
