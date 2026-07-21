//! Shared iroh plumbing: endpoint setup, addressing, and connection
//! observation. Used by the iroh throughput transport (`transport::iroh`) and,
//! in later phases, the `mux` lab. Only compiled with `--features iroh`.

pub mod endpoint;

/// ALPN for netsu's iperf3-shaped throughput test tunneled over one iroh/QUIC
/// connection. Distinct from the (later) mux and demo ALPNs so the protocols
/// can never cross-connect.
pub const THROUGHPUT_ALPN: &[u8] = b"netsu/iperf3-iroh/1";
