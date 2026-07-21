//! WebSocket transport [netsu extension]: the same protocol state machine
//! tunneled over WS binary frames. Ported from `packages/netsu/src/transport/ws.ts`;
//! see `PROTOCOL.md` "WebSocket mode".
//!
//! The design premise is that **WS binary frames are a byte pipe**: the byte
//! sequence on a WS control or data channel is byte-for-byte identical to the
//! TCP one (cookie, state bytes, length-prefixed JSON, payload) — no extra
//! framing, no per-message header, binary frames only. Fragmentation across WS
//! messages is arbitrary, so a `read_exact(n)` reassembles across as many
//! messages as it takes via a leftover-bytes buffer, exactly like `MemoryPipe`.
//!
//! [`WsPipe`] is generic over the underlying stream `S` so one type serves both
//! roles: the client's [`WsPipe::connect`] yields `WsPipe<MaybeTlsStream<TcpStream>>`
//! and the server's [`WsPipe::accept`] yields `WsPipe<TcpStream>`. Both satisfy
//! `BytePipe`, so [`crate::server::ServerCore::handle_connection`] drives a WS
//! connection through the same generic state machine as a TCP one.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, accept_async, connect_async};

use crate::error::{NetsuError, Result};
use crate::protocol::pipe::BytePipe;
use crate::streams::channel::DataChannel;

/// Default opening-handshake deadline. A peer that completes the TCP handshake
/// but never answers the HTTP Upgrade (a plain HTTP server, a hung proxy)
/// would otherwise wedge forever; `connect` wraps the handshake in
/// `tokio::time::timeout`, which is runtime-level and reliable (unlike the TS
/// version, which needed a second explicit timer because `ws`'s built-in
/// handshake timeout didn't fire on every runtime).
pub const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for the WS *closing* handshake. `WebSocketStream::close` drives the
/// handshake to completion — it sends a Close frame and then polls the stream
/// for the peer's Close reply. In forward mode the receiver (server) closes its
/// data stream while the peer is a pure *sender* that, at end-of-test, is no
/// longer reading its data socket: it will never send the Close reply, so an
/// unbounded `close().await` blocks forever, wedging the server's single-test
/// session (the whole server then rejects every later client as busy). We've
/// already received everything we are going to; bound the graceful close so a
/// non-reading peer can never wedge teardown. When the peer *is* reading
/// (reverse mode, or a well-behaved close), the handshake completes in well
/// under this cap; when it isn't, we fall through to dropping the stream, whose
/// TCP FIN the peer reads as end-of-stream just the same.
pub const WS_CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// Control-channel view of a WebSocket: reassembles the arbitrarily-fragmented
/// binary-frame byte stream so `read_exact` can pull out exactly `n` bytes.
pub struct WsPipe<S> {
    ws: WebSocketStream<S>,
    leftover: VecDeque<u8>,
}

impl WsPipe<MaybeTlsStream<TcpStream>> {
    /// Client side: open `ws://host:port/`, honoring `handshake_timeout` for
    /// the whole connect-and-upgrade so an unresponsive peer errors out rather
    /// than hanging.
    pub async fn connect(host: &str, port: u16, handshake_timeout: Duration) -> Result<Self> {
        let url = format!("ws://{host}:{port}/");
        let (ws, _resp) = tokio::time::timeout(handshake_timeout, connect_async(url))
            .await
            .map_err(|_| NetsuError::Timeout)?
            .map_err(|e| NetsuError::Protocol(format!("ws connect: {e}")))?;
        Ok(WsPipe {
            ws,
            leftover: VecDeque::new(),
        })
    }
}

impl WsPipe<TcpStream> {
    /// Server side: complete the WS opening handshake on an accepted TCP stream.
    pub async fn accept(stream: TcpStream) -> Result<Self> {
        let ws = accept_async(stream)
            .await
            .map_err(|e| NetsuError::Protocol(format!("ws accept: {e}")))?;
        Ok(WsPipe {
            ws,
            leftover: VecDeque::new(),
        })
    }
}

impl<S> WsPipe<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    /// Hands the connection to a [`WsDataChannel`] for the bulk payload. Errors
    /// if bytes are still buffered — the protocol guarantees the handshake
    /// leaves none, and discarding them would drop the start of the
    /// data-channel bytestream. (`WsPipe` consuming `self` here makes the
    /// "write/close after detach" guards the TS version needed unrepresentable,
    /// exactly as for `TcpPipe`.)
    pub fn into_data_channel(self) -> Result<WsDataChannel<S>> {
        if !self.leftover.is_empty() {
            return Err(NetsuError::Protocol(format!(
                "into_data_channel: {} buffered byte(s) would be lost",
                self.leftover.len()
            )));
        }
        Ok(WsDataChannel::new(self.ws))
    }

    // Inherent forwarders so callers can use `WsPipe` without importing the
    // `BytePipe` trait (same rationale as `TcpPipe`'s).

    pub async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        <Self as BytePipe>::read_exact(self, n, timeout).await
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        <Self as BytePipe>::write_all(self, data).await
    }

    pub async fn close(&mut self) {
        <Self as BytePipe>::close(self).await
    }

    /// Pulls binary frames until `leftover` holds at least `n` bytes, then
    /// drains exactly `n`. Non-binary control frames (ping/pong/text) are
    /// skipped; a close frame or stream end is EOF.
    async fn fill_and_drain(&mut self, n: usize) -> Result<Vec<u8>> {
        while self.leftover.len() < n {
            match self.ws.next().await {
                Some(Ok(Message::Binary(data))) => self.leftover.extend(data),
                Some(Ok(Message::Close(_))) | None => return Err(NetsuError::PipeClosed),
                Some(Ok(_)) => {} // ping/pong/text/frame: not payload, keep reading
                Some(Err(_)) => return Err(NetsuError::PipeClosed),
            }
        }
        Ok(self.leftover.drain(..n).collect())
    }
}

impl<S> BytePipe for WsPipe<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        match timeout {
            Some(d) => tokio::time::timeout(d, self.fill_and_drain(n))
                .await
                .map_err(|_| NetsuError::Timeout)?,
            None => self.fill_and_drain(n).await,
        }
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        // One binary frame per write. `send` flushes, so this is the
        // backpressure point, like TCP's `write_all`.
        self.ws
            .send(Message::Binary(data.to_vec()))
            .await
            .map_err(|e| NetsuError::Protocol(format!("ws send: {e}")))
    }

    async fn close(&mut self) {
        // Bounded — a peer not reading its socket must never wedge teardown
        // (see WS_CLOSE_TIMEOUT).
        let _ = tokio::time::timeout(WS_CLOSE_TIMEOUT, self.ws.close(None)).await;
    }
}

/// Bulk payload over a WebSocket. Like [`WsPipe`], reassembles binary frames so
/// a `read_chunk` can serve a caller whose buffer is smaller than an incoming
/// frame, and coalesces nothing on the send side (one frame per `write_chunk`).
pub struct WsDataChannel<S> {
    ws: WebSocketStream<S>,
    leftover: VecDeque<u8>,
    /// First failure latched so `error()` still reports it during result
    /// finalization even if the triggering call's own `Result` wasn't the one
    /// consulted at teardown — same rationale as `TcpDataChannel::error`.
    error: Option<NetsuError>,
}

impl<S> WsDataChannel<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    fn new(ws: WebSocketStream<S>) -> Self {
        WsDataChannel {
            ws,
            leftover: VecDeque::new(),
            error: None,
        }
    }

    fn latch(&mut self, err: &NetsuError) {
        self.error
            .get_or_insert_with(|| NetsuError::Protocol(err.to_string()));
    }

    // Inherent forwarders, as on `TcpDataChannel`.

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
impl<S> DataChannel for WsDataChannel<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        if let Some(err) = &self.error {
            return Err(NetsuError::Protocol(err.to_string()));
        }
        match self.ws.send(Message::Binary(chunk.to_vec())).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let err = NetsuError::Protocol(format!("ws send: {e}"));
                self.latch(&err);
                Err(err)
            }
        }
    }

    async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        if let Some(err) = &self.error {
            return Err(NetsuError::Protocol(err.to_string()));
        }
        loop {
            if !self.leftover.is_empty() {
                let n = self.leftover.len().min(buf.len());
                for (slot, b) in buf.iter_mut().zip(self.leftover.drain(..n)) {
                    *slot = b;
                }
                return Ok(n);
            }
            match self.ws.next().await {
                Some(Ok(Message::Binary(data))) => self.leftover.extend(data),
                Some(Ok(Message::Close(_))) | None => return Ok(0),
                Some(Ok(_)) => {} // ping/pong/text: not payload
                Some(Err(e)) => {
                    let err = NetsuError::Protocol(format!("ws recv: {e}"));
                    self.latch(&err);
                    return Err(err);
                }
            }
        }
    }

    async fn close(&mut self) {
        // Bounded for the same reason as `WsPipe::close` — the forward-mode
        // receiver closing its data stream must not block on a Close reply from
        // a peer that has stopped reading (see WS_CLOSE_TIMEOUT).
        let _ = tokio::time::timeout(WS_CLOSE_TIMEOUT, self.ws.close(None)).await;
    }

    fn error(&self) -> Option<&NetsuError> {
        self.error.as_ref()
    }
}
