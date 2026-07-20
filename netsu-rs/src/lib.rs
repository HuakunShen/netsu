//! netsu — an iperf3-compatible network speed test.
//!
//! The wire protocol is documented in `PROTOCOL.md` at the repository root and
//! is shared with the TypeScript implementation in `packages/netsu`.

pub mod client;
pub mod error;
pub mod protocol;
pub mod stats;
pub mod streams;
pub mod transport;

/// Crate version, sent on the wire as `client_version` during PARAM_EXCHANGE.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
