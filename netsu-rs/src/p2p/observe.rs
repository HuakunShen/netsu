//! Reads the selected-path type (direct vs relay) and RTT off a live iroh
//! `Connection`, for the result JSON and `--direct-only` enforcement.

use iroh::endpoint::Connection;

use crate::client::IrohConnectionInfo;

/// Snapshot the connection's currently selected path.
pub fn observe(connection: &Connection) -> IrohConnectionInfo {
    match connection.paths().iter().find(|p| p.is_selected()) {
        Some(path) => IrohConnectionInfo {
            observed_path: if path.is_relay() { "relay" } else { "direct" }.to_string(),
            rtt_us: Some(path.rtt().as_micros().min(u64::MAX as u128) as u64),
            remote_addr: Some(path.remote_addr().to_string()),
        },
        None => IrohConnectionInfo {
            observed_path: "unknown".to_string(),
            rtt_us: None,
            remote_addr: None,
        },
    }
}
