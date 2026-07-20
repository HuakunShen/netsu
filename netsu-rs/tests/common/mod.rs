//! Shared integration-test helpers: iperf3 availability, port allocation, and
//! spawning a real `iperf3 -s` referee process.
//!
//! Each integration-test binary `mod common;`-includes this file and uses a
//! different subset of it (e.g. `rs_to_rs` needs only `next_port`), so an
//! unused helper in any one binary is expected, not dead code.
#![allow(dead_code)]

use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

pub fn has_iperf3() -> bool {
    std::process::Command::new("iperf3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Ports 5310-5360. The TS suite owns 5210-5260; never use 5201.
static COUNTER: AtomicU16 = AtomicU16::new(0);

pub fn next_port() -> u16 {
    const BASE: u16 = 5310;
    const RANGE: u16 = 51;
    // Cargo runs each integration-test file in its own process, so a bare
    // counter collides across files. Offset by pid so concurrent binaries
    // start in different sub-windows.
    let pid_offset = (std::process::id() as u16) % RANGE;
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    BASE + (pid_offset + n) % RANGE
}

/// Spawn `iperf3 -s -1` (one-off). Resolves once the listening banner appears.
/// `--forceflush` is required: iperf3 block-buffers stdout through a pipe and
/// the banner would otherwise never arrive.
pub async fn spawn_iperf3_server(port: u16, extra: &[&str]) -> std::io::Result<Child> {
    let mut cmd = Command::new("iperf3");
    cmd.args(["-s", "-1", "-p", &port.to_string(), "--forceflush"])
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("Server listening") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !ready {
        let _ = child.kill().await;
        return Err(std::io::Error::other("iperf3 -s did not start"));
    }
    Ok(child)
}
