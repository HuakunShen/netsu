//! Normalizes Quinn diagnostics without making them throughput authority.

use std::time::Duration;

use crate::client::QuicConnectionInfo;

pub fn observe(
    connection: &quinn::Connection,
    handshake: Duration,
    verification: &'static str,
) -> QuicConnectionInfo {
    let stats = connection.stats();
    QuicConnectionInfo {
        handshake_ms: handshake.as_secs_f64() * 1000.0,
        rtt_us: Some(connection.rtt().as_micros().try_into().unwrap_or(u64::MAX)),
        remote_addr: Some(connection.remote_address().to_string()),
        certificate_verification: verification,
        lost_packets: Some(stats.path.lost_packets),
        congestion_events: Some(stats.path.congestion_events),
    }
}
