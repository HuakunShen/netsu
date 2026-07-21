use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::{NetsuError, Result};
use crate::streams::channel::DataChannel;

use super::pipe::{DataChannelSink, WebRtcAdapter, WebRtcInbound};

/// Bulk-stream adapter over a reliable ordered WebRTC DataChannel.
pub struct WebRtcChannel {
    adapter: WebRtcAdapter,
    latched_error: Option<NetsuError>,
}

impl WebRtcChannel {
    pub fn new(sink: Arc<dyn DataChannelSink>) -> (Self, WebRtcInbound) {
        let (adapter, inbound) = WebRtcAdapter::new(sink);
        (
            Self {
                adapter,
                latched_error: None,
            },
            inbound,
        )
    }

    pub async fn drain(&self) -> Result<()> {
        self.adapter.drain().await
    }

    pub fn backpressure_blocked(&self) -> Duration {
        self.adapter.backpressure_blocked()
    }

    fn latch(&mut self, detail: &'static str) {
        if self.latched_error.is_none() {
            self.latched_error = Some(NetsuError::Protocol(detail.into()));
        }
    }
}

#[async_trait]
impl DataChannel for WebRtcChannel {
    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        let result = self.adapter.write(chunk).await;
        if result.is_err() {
            self.latch("WebRTC DataChannel write failed");
        }
        result
    }

    async fn read_chunk(&mut self, target: &mut [u8]) -> Result<usize> {
        let result = self.adapter.read_up_to(target).await;
        if result.is_err() {
            self.latch("WebRTC DataChannel read failed");
        }
        result
    }

    async fn close(&mut self) {
        if self.adapter.close().await.is_err() {
            self.latch("WebRTC DataChannel close or drain failed");
        }
    }

    fn error(&self) -> Option<&NetsuError> {
        self.latched_error.as_ref()
    }
}
