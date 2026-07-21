use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify};
use tokio::time;

use crate::error::{NetsuError, Result};
use crate::protocol::pipe::BytePipe;

pub const MAX_DATA_CHANNEL_MESSAGE_BYTES: usize = 16 * 1_024;
pub const RECEIVE_QUEUE_LIMIT: usize = 1_024 * 1_024;
pub const SEND_HIGH_WATERMARK: usize = 4 * 1_024 * 1_024;
pub const SEND_LOW_WATERMARK: usize = 1_024 * 1_024;
pub const DATA_CHANNEL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimal transport surface required by the byte adapters.
///
/// `peer` implements this for webrtc-rs. Tests use a deterministic fake so
/// framing and backpressure behavior do not depend on a live SCTP stack.
#[async_trait]
pub trait DataChannelSink: Send + Sync {
    async fn send_binary(&self, data: &[u8]) -> Result<()>;
    async fn buffered_amount(&self) -> usize;
    async fn set_buffered_amount_low_threshold(&self, bytes: usize);
    async fn wait_buffered_amount_at_most(&self, maximum: usize);
    async fn close(&self) -> Result<()>;
}

struct InboundQueue {
    state: Mutex<InboundState>,
    changed: Notify,
}

struct InboundState {
    bytes: VecDeque<u8>,
    closed: bool,
    error: Option<&'static str>,
}

impl InboundQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(InboundState {
                bytes: VecDeque::new(),
                closed: false,
                error: None,
            }),
            changed: Notify::new(),
        })
    }

    async fn feed_binary(&self, bytes: &[u8]) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(NetsuError::PipeClosed);
        }
        let Some(new_len) = state.bytes.len().checked_add(bytes.len()) else {
            state.error = Some("WebRTC receive queue limit exceeded");
            state.closed = true;
            self.changed.notify_waiters();
            return Err(protocol_error("WebRTC receive queue limit exceeded"));
        };
        if new_len > RECEIVE_QUEUE_LIMIT {
            state.error = Some("WebRTC receive queue limit exceeded");
            state.closed = true;
            self.changed.notify_waiters();
            return Err(protocol_error("WebRTC receive queue limit exceeded"));
        }
        state.bytes.extend(bytes.iter().copied());
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    async fn feed_text(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.error = Some("WebRTC DataChannel text messages are unsupported");
        state.closed = true;
        drop(state);
        self.changed.notify_waiters();
        Err(protocol_error(
            "WebRTC DataChannel text messages are unsupported",
        ))
    }

    async fn fail(&self) {
        let mut state = self.state.lock().await;
        state.error = Some("WebRTC DataChannel transport failed");
        state.closed = true;
        drop(state);
        self.changed.notify_waiters();
    }

    async fn close(&self) {
        self.state.lock().await.closed = true;
        self.changed.notify_waiters();
    }

    async fn read_exact(&self, count: usize) -> Result<Vec<u8>> {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self.state.lock().await;
                if state.bytes.len() >= count {
                    return Ok(state.bytes.drain(..count).collect());
                }
                if let Some(error) = state.error {
                    return Err(protocol_error(error));
                }
                if state.closed {
                    return Err(NetsuError::PipeClosed);
                }
            }
            changed.await;
        }
    }

    async fn read_up_to(&self, target: &mut [u8]) -> Result<usize> {
        if target.is_empty() {
            return Ok(0);
        }
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self.state.lock().await;
                if !state.bytes.is_empty() {
                    let count = target.len().min(state.bytes.len());
                    for slot in &mut target[..count] {
                        *slot = state.bytes.pop_front().expect("length checked");
                    }
                    return Ok(count);
                }
                if let Some(error) = state.error {
                    return Err(protocol_error(error));
                }
                if state.closed {
                    return Ok(0);
                }
            }
            changed.await;
        }
    }
}

/// Callback-side handle used by the peer module to feed incoming messages.
#[derive(Clone)]
pub struct WebRtcInbound {
    queue: Arc<InboundQueue>,
}

impl WebRtcInbound {
    pub async fn feed_binary(&self, bytes: &[u8]) -> Result<()> {
        self.queue.feed_binary(bytes).await
    }

    pub async fn feed_text(&self, _text: &str) -> Result<()> {
        self.queue.feed_text().await
    }

    pub async fn fail(&self) {
        self.queue.fail().await;
    }

    pub async fn close(&self) {
        self.queue.close().await;
    }
}

pub(crate) struct WebRtcAdapter {
    sink: Arc<dyn DataChannelSink>,
    inbound: Arc<InboundQueue>,
    closed: bool,
    backpressure_blocked: Duration,
}

impl WebRtcAdapter {
    pub(crate) fn new(sink: Arc<dyn DataChannelSink>) -> (Self, WebRtcInbound) {
        let inbound = InboundQueue::new();
        (
            Self {
                sink,
                inbound: inbound.clone(),
                closed: false,
                backpressure_blocked: Duration::ZERO,
            },
            WebRtcInbound { queue: inbound },
        )
    }

    pub(crate) async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.closed {
            return Err(NetsuError::PipeClosed);
        }
        for message in bytes.chunks(MAX_DATA_CHANNEL_MESSAGE_BYTES) {
            self.wait_for_capacity().await;
            self.sink.send_binary(message).await?;
        }
        Ok(())
    }

    pub(crate) async fn read_exact(
        &self,
        count: usize,
        deadline: Option<Duration>,
    ) -> Result<Vec<u8>> {
        match deadline {
            Some(deadline) => time::timeout(deadline, self.inbound.read_exact(count))
                .await
                .map_err(|_| NetsuError::Timeout)?,
            None => self.inbound.read_exact(count).await,
        }
    }

    pub(crate) async fn read_up_to(&self, target: &mut [u8]) -> Result<usize> {
        self.inbound.read_up_to(target).await
    }

    pub(crate) async fn drain(&self) -> Result<()> {
        if self.sink.buffered_amount().await == 0 {
            return Ok(());
        }
        self.sink.set_buffered_amount_low_threshold(0).await;
        time::timeout(
            DATA_CHANNEL_DRAIN_TIMEOUT,
            self.sink.wait_buffered_amount_at_most(0),
        )
        .await
        .map_err(|_| protocol_error("WebRTC DataChannel drain timed out"))?;
        Ok(())
    }

    pub(crate) async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let drain = self.drain().await;
        let transport_close = self.sink.close().await;
        self.inbound.close().await;
        drain.and(transport_close)
    }

    pub(crate) fn backpressure_blocked(&self) -> Duration {
        self.backpressure_blocked
    }

    async fn wait_for_capacity(&mut self) {
        if self.sink.buffered_amount().await < SEND_HIGH_WATERMARK {
            return;
        }
        self.sink
            .set_buffered_amount_low_threshold(SEND_LOW_WATERMARK)
            .await;
        let started = Instant::now();
        self.sink
            .wait_buffered_amount_at_most(SEND_LOW_WATERMARK)
            .await;
        self.backpressure_blocked += started.elapsed();
    }
}

/// Control-channel byte-stream adapter over reliable ordered messages.
pub struct WebRtcPipe {
    adapter: WebRtcAdapter,
}

impl WebRtcPipe {
    pub fn new(sink: Arc<dyn DataChannelSink>) -> (Self, WebRtcInbound) {
        let (adapter, inbound) = WebRtcAdapter::new(sink);
        (Self { adapter }, inbound)
    }

    pub async fn drain(&self) -> Result<()> {
        self.adapter.drain().await
    }

    pub fn backpressure_blocked(&self) -> Duration {
        self.adapter.backpressure_blocked()
    }
}

impl BytePipe for WebRtcPipe {
    async fn read_exact(&mut self, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        self.adapter.read_exact(count, timeout).await
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.adapter.write(data).await
    }

    async fn close(&mut self) {
        let _ = self.adapter.close().await;
    }
}

fn protocol_error(detail: impl Into<String>) -> NetsuError {
    NetsuError::Protocol(detail.into())
}
