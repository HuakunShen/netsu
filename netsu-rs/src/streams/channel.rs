//! [`DataChannel`]: the bulk-payload abstraction used once a control
//! handshake hands its socket off for the actual test transfer (TCP/WS
//! bulk streams; UDP is packet-based and uses a separate abstraction).
//!
//! This is a plain `#[async_trait]` object rather than `BytePipe`'s native
//! async-fn-in-trait (RPITIT) style, because `DataChannel` **must** be
//! dyn-compatible: `src/streams/runner.rs` stores every open stream as
//! `Box<dyn DataChannel>` (see `SharedChannel`), and that's exactly what
//! `async_trait` buys here. `BytePipe`'s dyn-incompatibility, by contrast, is
//! a *consequence* of using RPITIT for its native async-fn-in-trait
//! ergonomics — not a goal in itself; RPITIT just doesn't produce a
//! dyn-compatible trait. `client.rs`'s transport-dispatch reasoning (see its
//! module doc) leans on this distinction: `BytePipe` has exactly one live
//! implementor and no need for dyn dispatch, while `DataChannel` is already
//! stored behind `Box<dyn ..>` today.

use async_trait::async_trait;

use crate::error::{NetsuError, Result};

/// Bulk payload channel for a data stream. Unlike `BytePipe`, it carries no
/// framing: chunks are opaque, and a transfer's own accounting (bytes,
/// duration, etc.) lives above this layer.
#[async_trait]
pub trait DataChannel: Send {
    /// Backpressure point: resolves once the transport has accepted `chunk`.
    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()>;

    /// Reads whatever is currently available into `buf`, returning the
    /// number of bytes read. `Ok(0)` signals a clean EOF.
    async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Sends any transport-specific ordered end marker after the final payload
    /// write. Byte-stream transports use socket close and need no marker.
    async fn finish_send(&mut self) -> Result<()> {
        Ok(())
    }

    /// WebRTC's SCTP mutation is not cancellation-safe. Such channels finish
    /// the current bounded write before observing the sender shutdown signal.
    fn finish_write_before_shutdown(&self) -> bool {
        false
    }

    /// Tears the channel down. Best-effort: failures here are not
    /// actionable by the caller and are not surfaced.
    async fn close(&mut self);

    /// A failure latched so it is still visible during result finalization,
    /// even for callers that observed a call succeed (or that never made a
    /// call at all) before the underlying transport failed out-of-band —
    /// e.g. a reader loop that only checks `Ok(0) | Err(_)` and discards the
    /// specific error, or a caller that wants to confirm no failure occurred
    /// at teardown time.
    fn error(&self) -> Option<&NetsuError>;
}
