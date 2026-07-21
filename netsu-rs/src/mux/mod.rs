//! The `netsu mux` lab: many prioritized, rate-limited streams over one iroh
//! connection, measuring whether a high-priority stream keeps low latency while
//! others load the link. A sibling of the iperf3 core (its own protocol,
//! config, and metrics), sharing only the `p2p` iroh plumbing. Only compiled
//! with `--features iroh`.
//!
//! Layers (bottom-up): [`config`] (what to run), [`workload`] (deterministic
//! paced payload generators), [`protocol`] (wire frames), [`metrics`]
//! (latency/fairness), then the runner/receiver engines.

pub mod config;
pub mod metrics;
pub mod protocol;
pub mod receiver;
pub mod runner;
pub mod workload;
