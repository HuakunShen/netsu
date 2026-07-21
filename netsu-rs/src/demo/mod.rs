//! Two-device keyboard/mouse sharing demo — the "perceived latency" (体感延迟)
//! tool. The controller captures real OS input and streams it to the
//! controlled peer over one iroh connection while pushing deterministic bulk
//! load, so you can *feel* input latency under load. Runnable only via
//! `examples/kbm-demo.rs` (feature `input-demo`), never the default binary.
//!
//! Safety: injection is opt-in (`--inject-input`), the peer is pinned
//! (`--allow-peer`), and `q` / Escape+Ctrl+Alt / idle all release every held
//! key. Roles are one-directional to avoid feedback loops.

pub mod input;
pub mod monio_backend;
pub mod session;

/// ALPN for the demo — distinct from the throughput and mux ALPNs.
pub const DEMO_ALPN: &[u8] = b"netsu/kbm-demo/1";
