//! Shared integration-test helpers: iperf3 availability, port allocation, and
//! spawning a real `iperf3 -s` referee process.
//!
//! Each integration-test binary `mod common;`-includes this file and uses a
//! different subset of it (e.g. `rs_to_rs` needs only `next_port`), so an
//! unused helper in any one binary is expected, not dead code.
#![allow(dead_code)]

use netsu::error::{NetsuError, Result};
use netsu::server::{ServerOptions, start_server};
use serde_json::Value;
use std::future::Future;
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

// ---------------------------------------------------------------------------
// UDP stream-setup retry harness
//
// The UDP stream setup — a client "hello" answered by a server "reply" — is a
// single unacknowledged datagram exchange with *no retransmission* on either
// side (`transport::udp::udp_client_connect` / `udp_server_accept`; netsu keeps
// this deliberately, to match iperf3 on the wire — see `server.rs`). So one
// setup datagram dropped by the loopback kernel — a couple percent of runs when
// idle, more under host load — fails the whole test *before any data is
// measured*:
//   * whoever sent the hello times out (netsu client: `NetsuError::Timeout`
//     after 5s; iperf3 client: a non-zero exit),
//   * and the *other* side is left blocked in its accept for up to 30s,
//     holding the netsu server's single active slot.
// That is a setup flake, not a measurement, so the honest fix is to retry the
// whole attempt — the packet/jitter/loss assertions then run, unweakened,
// against a real transfer. Because a dropped hello wedges the peer for ~30s,
// each retry must rebuild *everything* fresh (fresh `next_port()`, fresh
// server, fresh client); reusing a server across attempts would just meet a
// `ServerBusy` (netsu server) or a dead one-shot `iperf3 -s -1`.
// ---------------------------------------------------------------------------

/// Independent attempts per flaky UDP test. Attempts are independent Bernoulli
/// trials, so 4 turn a ~2% (idle) to ~20% (loaded) per-attempt setup-drop rate
/// into a ~1e-7 to ~1.6e-3 test-failure chance — robust without masking a
/// genuine, repeatable failure, which fails all 4 attempts and still surfaces.
pub const UDP_SETUP_ATTEMPTS: usize = 4;

/// Generous per-attempt wall-clock bound. Longer than any healthy attempt (the
/// slowest UDP test transfers for 3s), so it only ever fires on a *hang* — e.g.
/// an attempt stuck on the peer's 30s accept under pathological load — turning
/// that stall into a retryable transient instead of a ~30s (or, across attempts,
/// ~120s) wait. Never interrupts a legitimately running transfer.
const UDP_ATTEMPT_DEADLINE: Duration = Duration::from_secs(15);

/// Deadline for a single `iperf3` client invocation in [`retry_iperf3_to_netsu`].
/// Above the ~2s test + process startup, below the netsu server's 30s accept, so
/// a dropped one-shot hello is caught and retried in ~10s rather than ~30s.
const IPERF3_ATTEMPT_DEADLINE: Duration = Duration::from_secs(12);

/// Retry a whole **netsu-client** UDP attempt on *only* the pre-measurement
/// setup transient (a lost hello → [`NetsuError::Timeout`], or an attempt that
/// blows [`UDP_ATTEMPT_DEADLINE`]). Every other error — a protocol violation, a
/// wrong stream count, a genuinely wedged server — is returned immediately so
/// real regressions still fail fast on the first attempt.
///
/// The closure MUST rebuild the entire attempt each call — fresh server *and*
/// fresh client (see the module note above on why reuse fails). On the deadline
/// path the in-flight attempt future is dropped, which tears down its resources
/// (`iperf3 -s -1` is `kill_on_drop`; a netsu client/server future cancels).
pub async fn retry_udp_setup<T, F, Fut>(mut attempt: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    for n in 1..=UDP_SETUP_ATTEMPTS {
        let outcome = match tokio::time::timeout(UDP_ATTEMPT_DEADLINE, attempt()).await {
            Ok(r) => r,
            Err(_) => Err(NetsuError::Timeout), // hung past the deadline → transient
        };
        match outcome {
            Err(NetsuError::Timeout) if n < UDP_SETUP_ATTEMPTS => {
                eprintln!(
                    "udp stream setup timed out (attempt {n}/{UDP_SETUP_ATTEMPTS}); retrying"
                );
            }
            other => return other,
        }
    }
    unreachable!("loop returns on the final attempt")
}

/// Run `iperf3 -c … --json`, bounded by `deadline`. Returns `Some((code, json))`
/// if iperf3 exited in time, or `None` if it overran and was killed (a dropped
/// one-shot hello leaves iperf3 waiting on a reply that never comes). The child
/// is `kill_on_drop`, so the timeout's cancellation reaps it.
pub async fn run_iperf3_bounded(args: &[String], deadline: Duration) -> Option<(i32, Value)> {
    let child: Child = Command::new("iperf3")
        .args(args)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn iperf3");
    match tokio::time::timeout(deadline, child.wait_with_output()).await {
        Ok(out) => {
            let out = out.expect("iperf3 wait_with_output");
            let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
                panic!(
                    "iperf3 output not json: {e}\n{}",
                    String::from_utf8_lossy(&out.stdout)
                )
            });
            Some((out.status.code().unwrap_or(-1), json))
        }
        Err(_) => None,
    }
}

/// Retry harness for the **iperf3-client → netsu-server** tests. Each attempt
/// spins up a fresh netsu server on a fresh [`next_port`], builds the iperf3
/// args for that port via `args_for_port`, and runs iperf3 bounded by
/// [`IPERF3_ATTEMPT_DEADLINE`]; a clean exit (code 0) returns immediately, while
/// a non-zero exit or an overrun (the transient — iperf3's single hello was
/// dropped, leaving the server's accept hanging) tears the server down and
/// retries on a fresh one. A genuine, repeatable failure exhausts all attempts
/// and surfaces to the caller's `assert_eq!(code, 0, …)`.
pub async fn retry_iperf3_to_netsu<A>(mut args_for_port: A) -> (i32, Value)
where
    A: FnMut(u16) -> Vec<String>,
{
    let mut last = (-1, Value::Null);
    for n in 1..=UDP_SETUP_ATTEMPTS {
        let port = next_port();
        let server = start_server(ServerOptions {
            port,
            ..Default::default()
        })
        .await
        .expect("start netsu server");
        let outcome = run_iperf3_bounded(&args_for_port(port), IPERF3_ATTEMPT_DEADLINE).await;
        server.close().await;
        match outcome {
            Some((0, json)) => return (0, json),
            Some(other) => last = other,
            None => last = (-1, Value::Null),
        }
        if n < UDP_SETUP_ATTEMPTS {
            eprintln!("iperf3→netsu setup transient (attempt {n}/{UDP_SETUP_ATTEMPTS}); retrying");
        }
    }
    last
}

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
