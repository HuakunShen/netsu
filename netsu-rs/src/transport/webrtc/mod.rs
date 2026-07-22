//! Direct-only WebRTC DataChannel transport.
//!
//! Signaling exchanges only bounded SDP/ICE control frames. Benchmark payload
//! is accepted only after a direct selected candidate pair is proven; TURN is
//! intentionally unsupported.

pub mod channel;
pub mod config;
pub mod metrics;
pub mod peer;
pub mod pipe;
pub mod signaling;

pub use config::WebRtcOptions;
