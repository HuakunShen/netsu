# Native QUIC Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in, fixed-address native QUIC transport to `netsu-rs`, with one Quinn connection per test, the existing netsu control protocol over one bidirectional stream, parallel payload streams, TLS trust controls, diagnostics, local integration coverage, and Docker/netem E2E.

**Architecture:** Quinn provides the UDP/QUIC/TLS endpoint. The client opens one control bi-stream and `parallel` data bi-streams on a single connection; adapters implement the existing `BytePipe` and `DataChannel` contracts so the current lifecycle and accounting remain authoritative. QUIC-specific setup, TLS, connection ownership, and diagnostics stay in focused `transport/quic` modules and transport-specific client/server entry points modeled on the existing iroh path.

**Tech Stack:** Rust 2024, Tokio, Quinn `=0.11.11`, rustls `=0.23.42`, rcgen, rustls-pemfile, sha2, Clap, existing serde/serde_json, Bun container-matrix runner, Docker Compose, Linux `tc/netem`.

## Global Constraints

- Read `docs/specs/2026-07-22-quic-webrtc-transports.md` and `PROTOCOL.md` before editing.
- Do not modify `netsu-rs/src/tui.rs`; it has concurrent user work.
- Do not modify the TypeScript `packages/netsu` implementation for QUIC.
- `quic` remains opt-in; `default = []` must remain unchanged.
- ALPN is exactly `netsu/iperf3-quic/1`.
- One QUIC connection carries one control bi-stream and exactly `parallel` data bi-streams.
- Upload is default; `-R` and `-P` preserve existing meanings.
- `--udp` is mutually exclusive with `--quic`.
- No 0-RTT, session resumption, migration benchmark, datagrams, HTTP/3, or custom congestion-controller CLI in this plan.
- TLS verification is never silently disabled: client requires `--quic-ca` or `--quic-insecure`; server requires cert/key or `--quic-self-signed`.
- Throughput counts application payload only; handshake time is outside the measured interval.
- Automated tests assert correctness and bounded completion, never a minimum throughput.
- Preserve existing TCP/UDP/WS/iroh CLI and JSON behavior.
- Inspect `git status --short` before every commit and stage only files named by that task.

---

## File map

Create:

- `netsu-rs/src/transport/quic/mod.rs` — constants and public re-exports.
- `netsu-rs/src/transport/quic/tls.rs` — certificate loading/generation and client/server Quinn configs.
- `netsu-rs/src/transport/quic/channel.rs` — `QuicPipe` and `QuicChannel` adapters.
- `netsu-rs/src/transport/quic/endpoint.rs` — bounded endpoint bind/connect/accept and ownership.
- `netsu-rs/src/transport/quic/observe.rs` — Quinn stats normalization.
- `netsu-rs/tests/quic_transport.rs` — adapter/TLS/protocol failures.
- `netsu-rs/tests/quic_e2e.rs` — local client/server lifecycle matrix.
- `interop/transports/Dockerfile.rs` — extended-feature Rust image.
- `interop/transports/docker-compose.quic.yml` — isolated QUIC E2E topology.
- `interop/transports/e2e-quic.sh` — build/run/cleanup wrapper.
- `interop/transports/run-quic-matrix.ts` — correctness matrix driver.
- `interop/transports/netem-entrypoint.sh` — validated client-only impairment.
- `interop/transports/netem-profiles.json` — baseline/constrained/lossy profiles.
- `interop/transports/README.md` — extended transport harness usage.

Modify:

- `netsu-rs/Cargo.toml` and `netsu-rs/Cargo.lock` — optional dependencies/feature.
- `netsu-rs/src/transport/mod.rs` — compile-gated module.
- `netsu-rs/src/error.rs` — phase-tagged setup error.
- `netsu-rs/src/client.rs` — options, connection result, QUIC client path.
- `netsu-rs/src/server.rs` — options and QUIC accept path.
- `netsu-rs/src/main.rs` — CLI validation/output; never TUI.
- `netsu-rs/src/lib.rs` — expose compile-gated QUIC module through transport only.
- `netsu-rs/scripts/verify.sh` — feature verification.
- `PROTOCOL.md`, `README.md`, `netsu-rs/README.md`, `interop/README.md` — documentation.
- `package.json` — `e2e:quic` and later-compatible `e2e:transports` scripts.
- `.github/workflows/ci.yml` and `.github/workflows/e2e.yml` — feature and container gates.

---

### Task 1: Add the compile-gated QUIC feature and option types

**Files:**

- Modify: `netsu-rs/Cargo.toml`
- Modify: `netsu-rs/Cargo.lock`
- Modify: `netsu-rs/src/client.rs`
- Modify: `netsu-rs/src/server.rs`
- Modify: `netsu-rs/src/transport/mod.rs`
- Create: `netsu-rs/src/transport/quic/mod.rs`
- Test: `netsu-rs/tests/quic_transport.rs`

**Interfaces:**

- Produces: `Transport::Quic`, `QuicClientOptions`, `QuicServerOptions`, and `QUIC_ALPN`.
- `QuicClientOptions { insecure: bool, ca_path: Option<PathBuf> }`.
- `QuicServerOptions { self_signed: bool, cert_path: Option<PathBuf>, key_path: Option<PathBuf> }`.
- `ClientOptions.quic` and `ServerOptions.quic` default to `None`.
- `validate()` rejects a missing/mismatched transport option and contradictory TLS inputs.

- [ ] **Step 1: Write compile-gated validation tests**

Create `netsu-rs/tests/quic_transport.rs` with:

```rust
#![cfg(feature = "quic")]

use netsu::client::{ClientOptions, QuicClientOptions, Transport};
use netsu::server::{QuicServerOptions, ServerOptions};
use std::path::PathBuf;

#[test]
fn quic_client_requires_exactly_one_trust_mode() {
    let missing = ClientOptions { transport: Transport::Quic, ..Default::default() };
    assert!(missing.validate().unwrap_err().to_string().contains("QUIC client options"));

    let both = ClientOptions {
        transport: Transport::Quic,
        quic: Some(QuicClientOptions {
            insecure: true,
            ca_path: Some(PathBuf::from("ca.pem")),
        }),
        ..Default::default()
    };
    assert!(both.validate().unwrap_err().to_string().contains("exactly one"));
}

#[test]
fn quic_server_requires_self_signed_or_cert_key_pair() {
    let missing = ServerOptions { transport: Transport::Quic, ..Default::default() };
    assert!(missing.validate().is_err());

    let half_pair = ServerOptions {
        transport: Transport::Quic,
        quic: Some(QuicServerOptions {
            self_signed: false,
            cert_path: Some(PathBuf::from("cert.pem")),
            key_path: None,
        }),
        ..Default::default()
    };
    assert!(half_pair.validate().unwrap_err().to_string().contains("certificate and key"));
}

#[test]
fn quic_alpn_is_versioned_and_namespaced() {
    assert_eq!(netsu::transport::quic::QUIC_ALPN, b"netsu/iperf3-quic/1");
}
```

- [ ] **Step 2: Run the test to verify the feature and types are absent**

Run:

```bash
cd /Users/hk/Dev/netsu/netsu-rs
cargo test --features quic --test quic_transport
```

Expected: FAIL because feature `quic`, types, and module do not exist.

- [ ] **Step 3: Add dependencies, types, defaults, and validation**

Use exact dependency pins for already-resolved transport crates:

```toml
quic = [
  "dep:quinn", "dep:rustls", "dep:rustls-pemfile", "dep:rcgen", "dep:sha2",
]

quinn = { version = "=0.11.11", optional = true, default-features = false, features = ["runtime-tokio", "rustls-ring"] }
rustls = { version = "=0.23.42", optional = true, default-features = false, features = ["ring", "std"] }
rustls-pemfile = { version = "2", optional = true }
rcgen = { version = "0.14", optional = true, default-features = false, features = ["crypto", "ring"] }
sha2 = { version = "0.10", optional = true }
```

Add `#[cfg(feature = "quic")] pub mod quic;` and:

```rust
pub const QUIC_ALPN: &[u8] = b"netsu/iperf3-quic/1";
```

Implement validation before network I/O. Keep unrelated nested options absent:

```rust
if self.transport == Transport::Quic {
    let q = self.quic.as_ref().ok_or_else(|| NetsuError::Protocol("missing QUIC client options".into()))?;
    if q.insecure == q.ca_path.is_some() {
        return Err(NetsuError::Protocol("QUIC client requires exactly one of insecure or CA path".into()));
    }
} else if self.quic.is_some() {
    return Err(NetsuError::Protocol("QUIC options require Transport::Quic".into()));
}
```

- [ ] **Step 4: Verify default and QUIC feature builds**

Run:

```bash
cargo fmt --check
cargo check
cargo test --features quic --test quic_transport
cargo check --no-default-features --features quic
```

Expected: all exit 0; the default build does not compile Quinn.

- [ ] **Step 5: Commit only Task 1 files**

```bash
git add netsu-rs/Cargo.toml netsu-rs/Cargo.lock netsu-rs/src/client.rs netsu-rs/src/server.rs netsu-rs/src/transport/mod.rs netsu-rs/src/transport/quic/mod.rs netsu-rs/tests/quic_transport.rs
git diff --cached --check
git commit -m "feat(quic): add compile-gated transport configuration"
```

---

### Task 2: Add phase-tagged setup errors and normalized connection results

**Files:**

- Modify: `netsu-rs/src/error.rs`
- Modify: `netsu-rs/src/client.rs`
- Modify: `netsu-rs/src/main.rs`
- Test: `netsu-rs/tests/quic_transport.rs`

**Interfaces:**

- Produces: `SetupPhase`, `NetsuError::Setup`, `ConnectionInfo`, and `QuicConnectionInfo`.
- Preserves existing iroh JSON key values through the new enum.

- [ ] **Step 1: Add result/error serialization assertions**

Append tests that construct `QuicConnectionInfo`, call a small public
`connection_json(&ConnectionInfo)` helper, and assert:

```rust
assert_eq!(json["transport"], "quic");
assert_eq!(json["path"], "direct");
assert_eq!(json["certificate_verification"], "ca");
assert!(json.get("private_key").is_none());
assert!(json["handshake_ms"].as_f64().unwrap().is_finite());
```

Also snapshot the existing iroh representation before removing
`TestResult.iroh_connection`.

- [ ] **Step 2: Run the focused test and observe missing types**

Run `cargo test --features iroh,quic --test quic_transport`.

Expected: FAIL with unresolved `ConnectionInfo`/`QuicConnectionInfo`.

- [ ] **Step 3: Implement exact result types**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPhase { Resolve, Bind, Tls, QuicHandshake, ChannelsOpen }

#[derive(Debug, Clone)]
pub struct QuicConnectionInfo {
    pub handshake_ms: f64,
    pub rtt_us: Option<u64>,
    pub remote_addr: Option<String>,
    pub certificate_verification: &'static str,
    pub lost_packets: Option<u64>,
    pub congestion_events: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum ConnectionInfo {
    #[cfg(feature = "iroh")]
    Iroh(IrohConnectionInfo),
    #[cfg(feature = "quic")]
    Quic(QuicConnectionInfo),
}
```

Move CLI JSON construction behind one exhaustive helper. For Iroh, keep
`observed_path`, `rtt_us`, and `remote_addr`; add `transport: "iroh"` without
deleting old keys. For QUIC emit the spec fields and redact remote address by
default.

- [ ] **Step 4: Run regression and feature combinations**

```bash
cargo test
cargo test --features iroh,quic --test quic_transport
cargo clippy --all-targets --features iroh,quic -- -D warnings
```

Expected: all exit 0; existing iroh tests compile against the normalized field.

- [ ] **Step 5: Commit Task 2**

```bash
git add netsu-rs/src/error.rs netsu-rs/src/client.rs netsu-rs/src/main.rs netsu-rs/tests/quic_transport.rs
git diff --cached --check
git commit -m "refactor(result): normalize transport connection diagnostics"
```

---

### Task 3: Implement explicit QUIC TLS configuration

**Files:**

- Create: `netsu-rs/src/transport/quic/tls.rs`
- Modify: `netsu-rs/src/transport/quic/mod.rs`
- Test: `netsu-rs/tests/quic_transport.rs`

**Interfaces:**

- Produces `server_config(&QuicServerOptions) -> Result<(quinn::ServerConfig, CertificateMetadata)>`.
- Produces `client_config(&QuicClientOptions) -> Result<quinn::ClientConfig>`.
- Produces `CertificateMetadata { sha256: String, generated: bool }`.

- [ ] **Step 1: Write TLS behavior tests using a temporary directory**

Tests must cover:

```rust
#[tokio::test]
async fn generated_self_signed_requires_insecure_client() { /* verified CA client fails; insecure succeeds */ }

#[tokio::test]
async fn generated_test_ca_authenticates_server() { /* rcgen CA signs server; --quic-ca succeeds */ }

#[test]
fn malformed_pem_is_rejected_without_binding() { /* error contains phase=tls */ }
```

Use `std::env::temp_dir().join(format!("netsu-quic-pki-{}", uuid-like random u64))`
and remove only that exact directory at test end. Do not write keys into the
repository.

- [ ] **Step 2: Run TLS tests and confirm failure**

Run `cargo test --features quic --test quic_transport tls -- --nocapture`.

Expected: FAIL because `tls` functions are absent.

- [ ] **Step 3: Implement TLS loaders and self-signed generation**

Requirements:

- certificate and key PEM readers accept exactly one private key;
- CA mode populates an otherwise-empty rustls root store from the selected PEM;
- insecure verification is isolated in a type named `InsecureBenchmarkVerifier`;
- its module-level doc states it is used only after explicit CLI opt-in;
- server/client ALPN lists contain only `QUIC_ALPN`;
- self-signed certificate contains `localhost` and IP SANs `127.0.0.1`/`::1`;
- fingerprint is lowercase hex SHA-256 over DER;
- private key bytes never implement `Debug` or appear in errors.

- [ ] **Step 4: Verify TLS tests and lint**

```bash
cargo test --features quic --test quic_transport tls -- --nocapture
cargo clippy --all-targets --features quic -- -D warnings
```

Expected: all TLS cases pass; no warning about dangerous verification lacks an
explicit safety explanation.

- [ ] **Step 5: Commit Task 3**

```bash
git add netsu-rs/src/transport/quic/mod.rs netsu-rs/src/transport/quic/tls.rs netsu-rs/tests/quic_transport.rs
git diff --cached --check
git commit -m "feat(quic): add explicit TLS trust modes"
```

---

### Task 4: Implement Quinn endpoint ownership and channel adapters

**Files:**

- Create: `netsu-rs/src/transport/quic/endpoint.rs`
- Create: `netsu-rs/src/transport/quic/channel.rs`
- Modify: `netsu-rs/src/transport/quic/mod.rs`
- Test: `netsu-rs/tests/quic_transport.rs`

**Interfaces:**

- `QuicEndpoint::bind_server(SocketAddr, ServerConfig) -> Result<Self>`.
- `QuicEndpoint::bind_client(ClientConfig) -> Result<Self>`.
- `connect(&self, SocketAddr, server_name: &str) -> Result<(Connection, Duration)>`.
- `accept(&self) -> Result<(Connection, Duration)>`.
- `close(self)` waits at most two seconds.
- `QuicPipe::new(SendStream, RecvStream)` implements `BytePipe`.
- `QuicChannel::new(SendStream, RecvStream)` implements `DataChannel`.

- [ ] **Step 1: Write adapter and timeout tests**

Add tests equivalent to current `IrohPipe` tests:

```rust
#[tokio::test]
async fn quic_pipe_read_exact_spans_write_boundaries() {
    // peer writes [1,2] then [3,4,5]; reader asks for 5 and gets exact order
}

#[tokio::test]
async fn quic_channel_round_trips_payload_and_reports_eof() {
    // send 1 MiB in varying chunks; receiver count equals exactly 1 MiB
}

#[tokio::test]
async fn connect_to_unused_udp_port_times_out_in_quic_handshake_phase() {
    // bound under 12 seconds and error includes "quic handshake"
}
```

- [ ] **Step 2: Run focused tests to observe missing adapters**

Run `cargo test --features quic --test quic_transport quic_ -- --nocapture`.

Expected: FAIL because endpoint/channel modules do not exist.

- [ ] **Step 3: Implement endpoint and adapters**

Use constants:

```rust
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const STREAMS_TIMEOUT: Duration = Duration::from_secs(10);
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
```

`write_chunk` resolves after Quinn accepts the bytes. `read_chunk` maps clean
stream finish to `Ok(0)`. Latch the first transport error exactly like
`IrohChannel`. `close` calls `finish` and does not overwrite an earlier error.
Never hold a Tokio mutex while waiting for unrelated channel creation.

- [ ] **Step 4: Verify adapter behavior under repeated execution**

```bash
for i in 1 2 3 4 5; do cargo test --features quic --test quic_transport quic_; done
cargo clippy --all-targets --features quic -- -D warnings
```

Expected: five clean passes; timeout test completes within its outer bound.

- [ ] **Step 5: Commit Task 4**

```bash
git add netsu-rs/src/transport/quic/mod.rs netsu-rs/src/transport/quic/endpoint.rs netsu-rs/src/transport/quic/channel.rs netsu-rs/tests/quic_transport.rs
git diff --cached --check
git commit -m "feat(quic): add endpoint and stream adapters"
```

---

### Task 5: Bind the client state machine to one QUIC connection

**Files:**

- Modify: `netsu-rs/src/client.rs`
- Create: `netsu-rs/src/transport/quic/observe.rs`
- Modify: `netsu-rs/src/transport/quic/mod.rs`
- Test: `netsu-rs/tests/quic_e2e.rs`

**Interfaces:**

- Produces private `run_quic(host, opts, on_interval) -> Result<TestResult>`.
- Produces private `run_control_quic(connection, control, opts, on_interval)`
  using existing lifecycle helpers rather than a copied state machine.
- Produces `observe(&Connection, handshake, verification) -> QuicConnectionInfo`.

- [ ] **Step 1: Write client-side local pair tests**

Create `quic_e2e.rs`, initially with a small test acceptor that validates:

- first stream carries the 37-byte cookie;
- client waits for `CREATE_STREAMS` before opening data streams;
- exactly `parallel=4` streams open;
- reverse mode reads payload from the stream receive halves;
- unreachable server errors within 12 seconds.

- [ ] **Step 2: Run and confirm Transport::Quic is not dispatched**

Run `cargo test --features quic --test quic_e2e client_ -- --nocapture`.

Expected: FAIL because `run_client` has no QUIC arm.

- [ ] **Step 3: Implement QUIC client dispatch**

Resolve host with `tokio::net::lookup_host`, use the first address that
successfully handshakes within one overall ten-second deadline, open control,
send cookie, and drive the same PARAM/state/result functions. Open data streams
only after `CREATE_STREAMS`, send the cookie preamble on each, wrap each pair in
`QuicChannel`, and reuse `Session` sender/receiver bookkeeping.

Do not include resolution/handshake in `duration_seconds`. On every return path:

1. stop interval and duration tasks;
2. close channels;
3. send QUIC close if a connection exists;
4. close endpoint within two seconds;
5. preserve the original failure as the returned error.

- [ ] **Step 4: Verify focused client tests**

```bash
cargo test --features quic --test quic_e2e client_ -- --nocapture
cargo test --features quic --test quic_transport
```

Expected: all pass and no test exceeds its documented deadline.

- [ ] **Step 5: Commit Task 5**

```bash
git add netsu-rs/src/client.rs netsu-rs/src/transport/quic/mod.rs netsu-rs/src/transport/quic/observe.rs netsu-rs/tests/quic_e2e.rs
git diff --cached --check
git commit -m "feat(quic): run netsu client over Quinn streams"
```

---

### Task 6: Bind the server state machine to one QUIC connection

**Files:**

- Modify: `netsu-rs/src/server.rs`
- Test: `netsu-rs/tests/quic_e2e.rs`

**Interfaces:**

- `start_server` dispatches `Transport::Quic` before the TCP listener path.
- QUIC `NetsuServer.port` is the bound UDP port, including ephemeral port 0.
- QUIC accept loop handles sequential connections and the existing single-test lock.

- [ ] **Step 1: Write the four-cell and lifecycle tests**

Use one helper:

```rust
async fn run_case(reverse: bool, parallel: u32) {
    let server = start_quic_test_server(0).await;
    let result = run_quic_test_client(server.port, reverse, parallel).await.unwrap();
    assert!(result.sent_bytes > 0);
    assert!(result.received_bytes > 0);
    assert_eq!(result.local.streams.len(), parallel as usize);
    assert!(result.connection.is_some());
    server.close().await;
}
```

Add tests for `(false,1)`, `(true,1)`, `(false,4)`, `(true,4)`, two sequential
tests, concurrent server-busy, malformed cookie followed by a healthy run,
extra stream rejection, abrupt disconnect cleanup, and bounded server close
during an incomplete handshake.

- [ ] **Step 2: Run and confirm server dispatch is absent**

Run `cargo test --features quic --test quic_e2e -- --nocapture`.

Expected: FAIL before a test completes because server uses TCP path or lacks
QUIC dispatch.

- [ ] **Step 3: Implement QUIC accept/session ownership**

The accept loop owns the endpoint and a tracked set of connection tasks. For
each accepted connection:

1. accept the first bi-stream within ten seconds;
2. read/classify its cookie as the control stream;
3. enter existing `ServerCore` single-session logic;
4. after PARAM_EXCHANGE, accept exactly `parallel` data bi-streams within one
   ten-second deadline and verify each cookie;
5. reject extra/early/unidirectional streams with a namespaced application
   close code;
6. use the same `ServerSession` send/receive/result behavior;
7. release the active slot in a drop guard on every exit.

`NetsuServer::close` stops acceptance, closes connections, aborts only after the
bounded graceful wait, and releases the UDP port.

- [ ] **Step 4: Run local matrix repeatedly**

```bash
cargo test --features quic --test quic_e2e -- --nocapture
for i in 1 2 3; do cargo test --features quic --test quic_e2e four_cell; done
cargo clippy --all-targets --features quic -- -D warnings
```

Expected: all pass; second-client and abrupt-disconnect tests never poison the
next test.

- [ ] **Step 5: Commit Task 6**

```bash
git add netsu-rs/src/server.rs netsu-rs/tests/quic_e2e.rs
git diff --cached --check
git commit -m "feat(quic): accept netsu sessions over Quinn"
```

---

### Task 7: Add QUIC CLI flags and stable JSON/human output

**Files:**

- Modify: `netsu-rs/src/main.rs`
- Modify: `netsu-rs/tests/cli.rs`
- Test: `netsu-rs/tests/quic_e2e.rs`

**Interfaces:**

- Server flags: `--quic`, `--quic-self-signed`, `--quic-cert`, `--quic-key`.
- Client flags: `--quic`, `--quic-insecure`, `--quic-ca`.
- Feature-missing error: `quic support not compiled in; rebuild with --features quic`.

- [ ] **Step 1: Add CLI black-box tests**

Cover:

```text
client HOST --quic                         -> fail before network: trust mode missing
client HOST --quic --quic-ca a --quic-insecure -> fail: exactly one trust mode
server --quic                             -> fail: certificate mode missing
server --quic --quic-self-signed --quic-cert a --quic-key b -> fail
client HOST --quic --ws --quic-insecure   -> fail: mutually exclusive
client HOST --quic -u --quic-insecure     -> fail: reliable transport
```

Then spawn a self-signed QUIC server and run upload/reverse JSON clients. Assert
stdout parses, stderr contains only the insecure warning, and:

```rust
assert_eq!(json["connection"]["transport"], "quic");
assert_eq!(json["connection"]["path"], "direct");
assert!(json["connection"]["handshake_ms"].as_f64().unwrap() >= 0.0);
```

- [ ] **Step 2: Run CLI tests and observe unknown flags**

Run `cargo test --features quic --test cli -- quic --nocapture`.

Expected: FAIL with unknown `--quic` or absent routing.

- [ ] **Step 3: Implement selection and output**

Replace the two-boolean `select_transport` with one validation function that
counts `ws`, `iroh`, `quic`, and later `webrtc` booleans. Keep every flag
recognized even when its feature is absent. Build nested options only for the
selected transport. Human output includes handshake and RTT; JSON remains pure
stdout except that explicit insecure warnings go to stderr.

- [ ] **Step 4: Run CLI and legacy regression tests**

```bash
cargo test --features ws,iroh,quic --test cli -- --nocapture
cargo test --features ws,iroh,quic
cargo clippy --all-targets --features ws,iroh,quic -- -D warnings
```

Expected: all pass; existing CLI forms remain valid.

- [ ] **Step 5: Commit Task 7**

```bash
git add netsu-rs/src/main.rs netsu-rs/tests/cli.rs netsu-rs/tests/quic_e2e.rs
git diff --cached --check
git commit -m "feat(cli): expose native QUIC benchmark flags"
```

---

### Task 8: Add the isolated Docker/netem QUIC matrix

**Files:**

- Create: `interop/transports/Dockerfile.rs`
- Create: `interop/transports/docker-compose.quic.yml`
- Create: `interop/transports/e2e-quic.sh`
- Create: `interop/transports/run-quic-matrix.ts`
- Create: `interop/transports/netem-entrypoint.sh`
- Create: `interop/transports/netem-profiles.json`
- Create: `interop/transports/README.md`
- Modify: `package.json`

**Interfaces:**

- `bun run e2e:quic` builds, runs all cells, writes failure artifacts, and
  tears down with `--remove-orphans`.
- Runner accepts `QUIC_CASE` to select one cell for debugging.

- [ ] **Step 1: Write matrix construction tests before Docker execution**

Export `buildQuicMatrix()` and test with `bun:test` that it returns exactly:

```text
baseline upload P1
baseline reverse P1
baseline upload P4
baseline reverse P4
constrained upload P1
lossy upload P1
```

Assert every cell has a unique name and timeout `(duration + 25) * 1000`.

- [ ] **Step 2: Run matrix unit test and observe missing runner**

Run `bun test interop/transports/run-quic-matrix.test.ts`.

Expected: FAIL because files/functions are absent.

- [ ] **Step 3: Implement image, topology, netem, and runner**

The Rust image builds `--features quic,webrtc` only after the later WebRTC plan;
during this plan use `--features quic`. Compose uses an idle server/client image
on one bridge. The runner starts a unique self-signed server per cell, waits for
an explicit readiness line, executes a JSON client with `--quic-insecure`, and
kills/reaps the server in `finally`.

Validate every netem value against anchored unit-bearing regexes before passing
it to `tc`; apply netem only in the client container. Profiles are exactly:

```json
{
  "baseline":{"rate":"500mbit","delay":"10ms","jitter":"0ms","loss":"0%"},
  "constrained":{"rate":"100mbit","delay":"50ms","jitter":"5ms","loss":"0.1%"},
  "lossy":{"rate":"100mbit","delay":"20ms","jitter":"0ms","loss":"2%"}
}
```

Assertions: exit 0, valid JSON, positive finite rate below 1e12, non-zero sent/
received bytes, drift <=2%, correct stream count, QUIC/direct path, and bounded
handshake. Do not assert a minimum rate.

- [ ] **Step 4: Run matrix unit tests then real Docker E2E**

```bash
bun test interop/transports/run-quic-matrix.test.ts
bun run e2e:quic
docker compose -f interop/transports/docker-compose.quic.yml ps -a
```

Expected: unit and E2E exit 0; final `ps -a` shows no project resources because
the wrapper ran `down -v --remove-orphans`.

- [ ] **Step 5: Commit Task 8**

```bash
git add package.json interop/transports
git diff --cached --check
git commit -m "test(quic): add Docker netem correctness matrix"
```

---

### Task 9: Document the QUIC binding and add CI gates

**Files:**

- Modify: `PROTOCOL.md`
- Modify: `README.md`
- Modify: `netsu-rs/README.md`
- Modify: `interop/README.md`
- Modify: `netsu-rs/scripts/verify.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/e2e.yml`

- [ ] **Step 1: Add a documentation/CLI consistency test**

Extend CLI tests to run `netsu server --help` and `netsu client --help`, asserting
every documented QUIC flag appears. Add a protocol constant test asserting the
documented ALPN literal matches `QUIC_ALPN`.

- [ ] **Step 2: Run the test before docs/CI edits**

Run `cargo test --features quic --test cli quic_help`.

Expected: PASS only if Task 7 flags exist; record this as the baseline before
documentation changes.

- [ ] **Step 3: Write docs and CI steps**

Document exact commands:

```bash
cargo run --features quic -- server --quic --quic-self-signed -p 5201
cargo run --features quic -- client 127.0.0.1 --quic --quic-insecure -p 5201 -t 10 -P 4
```

State that official iperf3 cannot interoperate, insecure mode is explicit, and
container throughput is not a benchmark result. CI adds:

```yaml
- run: cargo clippy --all-targets --features quic -- -D warnings
  working-directory: netsu-rs
- run: cargo test --features quic
  working-directory: netsu-rs
```

The E2E workflow adds a separate `quic-transport` job invoking
`bun run e2e:quic`, `timeout-minutes: 30`, with `if: failure()` log dumping.

- [ ] **Step 4: Verify docs and workflow syntax cheaply**

```bash
rg -n "netsu/iperf3-quic/1|quic-insecure|e2e:quic" PROTOCOL.md README.md netsu-rs/README.md interop/README.md package.json .github/workflows
git diff --check
cargo test --features quic --test cli
```

Expected: literals appear in their intended files, no whitespace errors, tests
pass.

- [ ] **Step 5: Commit Task 9**

```bash
git add PROTOCOL.md README.md netsu-rs/README.md interop/README.md netsu-rs/scripts/verify.sh .github/workflows/ci.yml .github/workflows/e2e.yml netsu-rs/tests/cli.rs
git diff --cached --check
git commit -m "docs(ci): gate the native QUIC transport"
```

---

### Task 10: Run final QUIC verification and inspect artifacts

**Files:**

- No product edits expected; fix only failures proven by these commands.

- [ ] **Step 1: Inspect scope before verification**

```bash
git status --short
git diff --check
git log -10 --oneline
```

Expected: only pre-existing user changes remain unstaged; QUIC work is committed
in the scoped commits above.

- [ ] **Step 2: Run Rust default and feature gates**

```bash
cd /Users/hk/Dev/netsu/netsu-rs
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo clippy --all-targets --features ws,iroh,quic,tui -- -D warnings
cargo test --features ws,iroh,quic,tui
cargo build --release
cargo build --release --features quic
cargo publish --dry-run
```

Expected: every command exits 0. Do not interpret a feature build as proof of
container or network behavior.

- [ ] **Step 3: Run existing and new container gates**

```bash
cd /Users/hk/Dev/netsu
bun run e2e
bun run e2e:quic
```

Expected: both matrices report zero failed cells and tear down their projects.

- [ ] **Step 4: Inspect results rather than trusting exit status alone**

```bash
rg -n '"transport":"quic"|"path":"direct"|"handshake_ms"' interop/transports/results
find interop/transports/results -type f -maxdepth 3 -print
git status --short
```

Expected: every retained JSON artifact has QUIC/direct diagnostics; no secret,
PEM, or key file appears; the final status does not contain generated results.

- [ ] **Step 5: Record manual validation as unresolved evidence**

In the handoff, explicitly state that Docker/netem proves controlled protocol
behavior, not physical LAN/Wi-Fi/WAN performance. List the four manual scenarios
from the spec as remaining release evidence unless they were actually run.

---

## Execution handoff

Implement this plan before the WebRTC plan so the shared `ConnectionInfo`, CLI
selection, extended Docker directory, and reliable multiplexed-transport
patterns are stable. Use one agent/task at a time unless independent tasks are
assigned separate worktrees; this checkout already contains user modifications,
so execution must preserve them and must never stage `netsu-rs/src/tui.rs`.
