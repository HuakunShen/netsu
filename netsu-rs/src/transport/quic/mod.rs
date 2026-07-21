//! Fixed-address native QUIC transport built directly on Quinn.
//!
//! This transport is separate from iroh: callers provide a host and UDP port,
//! and must explicitly select either CA verification or benchmark-only
//! insecure verification. The existing netsu control/data state machines stay
//! above this module.

pub mod tls;

/// Versioned ALPN for the native QUIC binding.
pub const QUIC_ALPN: &[u8] = b"netsu/iperf3-quic/1";
