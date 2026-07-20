# netsu Rust Implementation (Phase 2 of 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust implementation of netsu that speaks the same iperf3 wire protocol as the TypeScript one, verified against official iperf3 on localhost.

**Architecture:** Mirror the TS module layout so understanding one implementation means understanding the other. Control state machine over a transport-agnostic byte-pipe trait; data plane in separate connections; pure-logic `protocol/` and `stats` modules with no socket access.

**Tech Stack:** Rust 2024 edition, tokio (async runtime), tokio-tungstenite (WS), serde/serde_json, clap (CLI), thiserror (lib errors) + anyhow (CLI). Official iperf3 binary as test referee.

## The two authorities — read both before starting any task

1. **`/Users/hk/Dev/netsu/PROTOCOL.md`** — the wire protocol. It is the cross-implementation source of truth and was corrected twice during Phase 1 against the real iperf3 binary. Where this plan's prose and PROTOCOL.md disagree, PROTOCOL.md wins; report the disagreement.

2. **`/Users/hk/Dev/netsu/packages/netsu/src/`** — the TypeScript implementation. It is complete, reviewed, and verified interoperable with official iperf3 over TCP and UDP in both directions. **It is a reference implementation, not a style guide.** Port its *behavior* and its hard-won edge-case handling; write idiomatic Rust, not transliterated TypeScript.

Every task below names the TS file(s) that already solve the same problem. Read them. The comments in those files explain *why* non-obvious things are the way they are — several document bugs that took real debugging to find.

## Global Constraints

- Crate name `netsu` (verified available on crates.io 2026-07-20), version `0.2.0`, edition 2024, license MIT. Lives in `netsu-rs/` (directory name stays; package name is `netsu`).
- Binary target `netsu` + library target `netsu`.
- `cargo clippy --all-targets -- -D warnings` must pass at every commit. `cargo fmt --check` must pass.
- `src/protocol/` and `src/stats.rs` must never reference `tokio::net`, `std::net`, or any socket type. They are pure logic reached only through the byte-pipe trait. This boundary is what made the TS implementation unit-testable; it is not optional.
- No `unsafe`. No `.unwrap()` / `.expect()` in library code outside tests (use `?` and typed errors); `unwrap` in tests is fine.
- Tests use ports 5310–5360 (the TS suite owns 5210–5260 — do not collide, both suites may run concurrently in CI).
- Integration tests against official iperf3 are gated on the binary being present at runtime; they must skip cleanly, not fail, when it is absent.
- Every spawned iperf3 process is killed on both the success and failure path.
- Conventional commits. Commit after every task.
- Run commands from `netsu-rs/` unless stated otherwise.

## Protocol facts that cost Phase 1 real debugging time

These are in PROTOCOL.md, but they are the ones an implementer is most likely to get wrong. Each caused a failing interop test in Phase 1:

1. **Stream ids are `1, 3, 4, 5, ..., N+1` — not `1..N`.** An `iperf_add_stream` quirk. Ids never travel on the wire, so both peers must derive them identically or iperf3 rejects the results with "stream has an invalid id". See `packages/netsu/src/streams/runner.ts`'s `nextStreamId` and its comment.
2. **The UDP handshake constants are raw wire bytes, not integers to byte-swap.** Hello is `39 38 37 36` (ASCII `"9876"`), reply is `36 37 38 39` (ASCII `"6789"`). iperf3 writes its `UDP_CONNECT_MSG`/`UDP_CONNECT_REPLY` constants un-byte-swapped and `iperf.h` picks a different C literal per host endianness so the wire bytes stay fixed. **Do not apply `htonl` to the values printed in iperf3's source.** PROTOCOL.md's "UDP specifics" says this explicitly.
3. **The client sends its results FIRST during EXCHANGE_RESULTS**, then reads the server's.
4. **The client sends `TEST_END` on the control channel BEFORE tearing down its data streams**, and in reverse mode leaves its receive streams open until final cleanup — otherwise it RSTs the sending peer mid-write and both sides mis-report.
5. **UDP block size can exceed what the host can actually send.** iperf3 negotiates `blksize` from the path MTU (16332 on loopback); macOS caps UDP datagrams at `net.inet.udp.maxdgram` (9216). Probe the real maximum with an actual send; do not trust a socket-option getter. See `packages/netsu/src/transport/udp.ts`.
6. **Only count bytes the kernel accepted.** Credit the counter on send success, not on attempt — but keep the packet sequence number advancing per attempt, so the receiver's `max_pcount - received` loss math stays correct.
7. **A UDP send error must not abort the test.** Count it and continue, as iperf3 does. A TCP write failure *is* fatal — keep that asymmetry.

---

### Task 1: Crate scaffold, toolchain, and CI-ready lint gates

Replace the hello-world shell with a real lib+bin crate that builds, tests, lints, and formats clean.

**Files:**
- Modify: `netsu-rs/Cargo.toml`
- Create: `netsu-rs/src/lib.rs`, `netsu-rs/src/main.rs` (replace), `netsu-rs/.gitignore`, `netsu-rs/rust-toolchain.toml`
- Delete: `netsu-rs/Cargo.lock` is kept (binary crate — lockfile is committed)

**Interfaces:**
- Consumes: nothing
- Produces: a crate named `netsu` at 0.2.0 exposing an empty `lib.rs`, and a `netsu` binary that runs

- [ ] **Step 1: Verify toolchain**

```bash
cargo --version   # 1.96+
rustc --version
iperf3 --version  # 3.x, the test referee
```

- [ ] **Step 2: Rewrite Cargo.toml**

```toml
[package]
name = "netsu"
version = "0.2.0"
edition = "2024"
license = "MIT"
description = "iperf3-compatible network speed test — library and CLI"
repository = "https://github.com/HuakunShen/netsu"
readme = "README.md"

[lib]
name = "netsu"
path = "src/lib.rs"

[[bin]]
name = "netsu"
path = "src/main.rs"

[dependencies]
tokio = { version = "1", features = ["net", "rt-multi-thread", "macros", "time", "io-util", "sync", "signal"] }
tokio-tungstenite = "0.24"
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
thiserror = "2"
anyhow = "1"
rand = "0.8"

[dev-dependencies]
tokio = { version = "1", features = ["full"] }
```

- [ ] **Step 3: Pin the toolchain**

`netsu-rs/rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

- [ ] **Step 4: Add .gitignore**

`netsu-rs/.gitignore`:

```
/target
```

- [ ] **Step 5: Placeholder lib and bin**

`netsu-rs/src/lib.rs`:

```rust
//! netsu — an iperf3-compatible network speed test.
//!
//! The wire protocol is documented in `PROTOCOL.md` at the repository root and
//! is shared with the TypeScript implementation in `packages/netsu`.

/// Crate version, sent on the wire as `client_version` during PARAM_EXCHANGE.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

`netsu-rs/src/main.rs`:

```rust
fn main() {
    println!("netsu {}", netsu::VERSION);
}
```

- [ ] **Step 6: Verify build, test, lint, format**

```bash
cd /Users/hk/Dev/netsu/netsu-rs
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): scaffold netsu rust crate with lint gates"
```

---

### Task 2: Protocol core — states, cookie, byte pipe, framing

Pure logic, no sockets. The `BytePipe` trait and `MemoryPipe` double are what every later unit test builds on.

**Reference:** `packages/netsu/src/protocol/states.ts`, `cookie.ts`, `pipe.ts`, `framing.ts`. PROTOCOL.md sections "Cookie", "State bytes", "JSON framing".

**Files:**
- Create: `netsu-rs/src/protocol/mod.rs`, `states.rs`, `cookie.rs`, `pipe.rs`, `framing.rs`
- Create: `netsu-rs/src/error.rs`
- Modify: `netsu-rs/src/lib.rs` (add `pub mod`s)

**Interfaces:**
- Consumes: nothing
- Produces:
  - `error.rs`: `#[derive(Debug, thiserror::Error)] pub enum NetsuError` with at minimum variants `Io(#[from] std::io::Error)`, `Json(#[from] serde_json::Error)`, `Protocol(String)`, `Timeout`, `PipeClosed`, `ServerBusy`, `ServerError`; `pub type Result<T> = std::result::Result<T, NetsuError>`
  - `states.rs`: `pub const TEST_START: i8 = 1; TEST_RUNNING: i8 = 2; TEST_END: i8 = 4; PARAM_EXCHANGE: i8 = 9; CREATE_STREAMS: i8 = 10; SERVER_TERMINATE: i8 = 11; CLIENT_TERMINATE: i8 = 12; EXCHANGE_RESULTS: i8 = 13; DISPLAY_RESULTS: i8 = 14; IPERF_START: i8 = 15; IPERF_DONE: i8 = 16; ACCESS_DENIED: i8 = -1; SERVER_ERROR: i8 = -2;` and `pub const COOKIE_SIZE: usize = 37;`
  - `cookie.rs`: `pub fn make_cookie() -> String` (36 chars), `pub fn cookie_to_bytes(c: &str) -> [u8; COOKIE_SIZE]`, `pub fn bytes_to_cookie(b: &[u8]) -> String`
  - `pipe.rs`: `#[async_trait] pub trait BytePipe: Send { async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>>; async fn write_all(&mut self, data: &[u8]) -> Result<()>; async fn close(&mut self); }` and `pub struct MemoryPipe` with `pub fn pair() -> (MemoryPipe, MemoryPipe)`
  - `framing.rs`: `pub async fn write_state<P: BytePipe>(p: &mut P, state: i8) -> Result<()>`, `pub async fn read_state<P: BytePipe>(p: &mut P, timeout: Option<Duration>) -> Result<i8>`, `pub async fn write_json<P: BytePipe, T: Serialize>(p: &mut P, v: &T) -> Result<()>`, `pub async fn read_json<P: BytePipe, T: DeserializeOwned>(p: &mut P, max: usize, timeout: Option<Duration>) -> Result<T>` with `pub const MAX_JSON: usize = 65536`

Note: states are `i8` because iperf3 writes a **signed** byte; `ACCESS_DENIED` is `0xFF` on the wire and must read back as `-1`. Using `u8` here is the classic bug.

If `async_trait` is needed for object safety, add `async-trait = "0.1"` to dependencies. Prefer native async-in-trait if the pinned toolchain supports it for your usage; either is acceptable — state which you chose and why.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/src/protocol/cookie.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_36_char_cookies_from_the_iperf3_alphabet() {
        let c = make_cookie();
        assert_eq!(c.len(), 36);
        assert!(c.chars().all(|ch| "abcdefghijklmnopqrstuvwxyz234567".contains(ch)));
        assert_ne!(make_cookie(), c);
    }

    #[test]
    fn round_trips_through_37_byte_nul_terminated_wire_form() {
        let c = make_cookie();
        let b = cookie_to_bytes(&c);
        assert_eq!(b.len(), COOKIE_SIZE);
        assert_eq!(b[36], 0);
        assert_eq!(bytes_to_cookie(&b), c);
    }
}
```

`netsu-rs/src/protocol/framing.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::pipe::MemoryPipe;
    use crate::protocol::states::{ACCESS_DENIED, PARAM_EXCHANGE};

    #[tokio::test]
    async fn round_trips_positive_and_negative_state_bytes() {
        let (mut a, mut b) = MemoryPipe::pair();
        write_state(&mut a, PARAM_EXCHANGE).await.unwrap();
        write_state(&mut a, ACCESS_DENIED).await.unwrap();
        assert_eq!(read_state(&mut b, None).await.unwrap(), PARAM_EXCHANGE);
        // signed: 0xff must read back as -1, not 255
        assert_eq!(read_state(&mut b, None).await.unwrap(), ACCESS_DENIED);
    }

    #[tokio::test]
    async fn round_trips_json_with_4_byte_be_length_prefix() {
        let (mut a, mut b) = MemoryPipe::pair();
        let msg = serde_json::json!({ "tcp": true, "time": 10, "parallel": 2 });
        write_json(&mut a, &msg).await.unwrap();
        let got: serde_json::Value = read_json(&mut b, MAX_JSON, None).await.unwrap();
        assert_eq!(got, msg);
    }

    #[tokio::test]
    async fn rejects_json_larger_than_max() {
        let (mut a, mut b) = MemoryPipe::pair();
        let msg = serde_json::json!({ "pad": "x".repeat(100) });
        write_json(&mut a, &msg).await.unwrap();
        let got = read_json::<_, serde_json::Value>(&mut b, 50, None).await;
        assert!(got.is_err());
    }
}
```

`netsu-rs/src/protocol/pipe.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delivers_written_bytes_respecting_chunk_boundaries() {
        let (mut a, mut b) = MemoryPipe::pair();
        a.write_all(&[1, 2, 3, 4, 5]).await.unwrap();
        assert_eq!(b.read_exact(2, None).await.unwrap(), vec![1, 2]);
        assert_eq!(b.read_exact(3, None).await.unwrap(), vec![3, 4, 5]);
    }

    #[tokio::test]
    async fn read_exact_waits_for_enough_bytes() {
        let (mut a, mut b) = MemoryPipe::pair();
        let task = tokio::spawn(async move { b.read_exact(4, None).await });
        a.write_all(&[9]).await.unwrap();
        a.write_all(&[8, 7, 6]).await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), vec![9, 8, 7, 6]);
    }

    #[tokio::test]
    async fn read_exact_errors_on_close() {
        let (mut a, mut b) = MemoryPipe::pair();
        let task = tokio::spawn(async move { b.read_exact(1, None).await });
        a.close().await;
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn read_exact_honors_its_timeout() {
        let (_a, mut b) = MemoryPipe::pair();
        let got = b.read_exact(1, Some(Duration::from_millis(50))).await;
        assert!(matches!(got, Err(crate::error::NetsuError::Timeout)));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/hk/Dev/netsu/netsu-rs && cargo test
```
Expected: FAIL — modules do not exist.

- [ ] **Step 3: Implement**

Port the behavior from the TS reference files named above. The cookie alphabet is `abcdefghijklmnopqrstuvwxyz234567` (32 chars, so `byte % 32` has no modulo bias — keep that property). JSON framing is `[u32 big-endian length][UTF-8 JSON]`.

`MemoryPipe` needs a shared buffer with a waiter, like TS's `ByteBuffer`. In Rust the idiomatic shape is a `tokio::sync::mpsc` channel plus a leftover-bytes buffer, or an `Arc<Mutex<VecDeque<u8>>>` with a `Notify`. Either is fine; the required behaviors are the four tests above.

The `read_exact` timeout should be implemented with `tokio::time::timeout` and map elapsed to `NetsuError::Timeout`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): protocol core - states, cookie, byte pipe, framing"
```

---

### Task 3: Params and results codecs

Typed serde structs for the two JSON control messages, field names exactly as PROTOCOL.md.

**Reference:** `packages/netsu/src/protocol/params.ts`, `results.ts`. PROTOCOL.md sections "PARAM_EXCHANGE JSON" and "EXCHANGE_RESULTS JSON".

**Files:**
- Create: `netsu-rs/src/protocol/params.rs`, `netsu-rs/src/protocol/results.rs`
- Modify: `netsu-rs/src/protocol/mod.rs`

**Interfaces:**
- Consumes: `error::Result`
- Produces:
  - `params.rs`: `pub struct TestParams { pub udp: bool, pub time: u32, pub parallel: u32, pub len: usize, pub reverse: bool, pub bandwidth: u64 }`; `pub fn encode(p: &TestParams) -> serde_json::Value`; `pub fn decode(v: serde_json::Value) -> Result<TestParams>`; `pub const DEFAULT_TCP_LEN: usize = 131072; DEFAULT_UDP_LEN: usize = 1460; DEFAULT_UDP_BANDWIDTH: u64 = 1048576; MAX_PARALLEL: u32 = 128; MAX_LEN: usize = 1048576; MAX_TIME: u32 = 86400;`
  - `results.rs`: `pub struct StreamResult { pub id: u32, pub bytes: u64, pub retransmits: i64, pub jitter: f64, pub errors: u64, pub packets: u64, pub start_time: f64, pub end_time: f64 }`; `pub struct EndResults { pub sender_has_retransmits: i64, pub streams: Vec<StreamResult> }`; `pub fn encode(r: &EndResults) -> serde_json::Value`; `pub fn decode(v: serde_json::Value) -> Result<EndResults>`

Encoding rules that must hold (each is asserted below):
- Exactly one of `tcp` / `udp` is present. Decoding must **reject both-present and neither-present** — the TS implementation shipped a bug here where both-present silently resolved to udp.
- `reverse` is present only when true.
- `bandwidth` is present only for UDP.
- Unknown incoming fields are tolerated (iperf3 sends many more than netsu reads).
- On the wire, jitter is in **seconds**; `retransmits` is `-1` when unavailable.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/src/protocol/params.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TestParams {
        TestParams { udp: false, time: 10, parallel: 2, len: 131072, reverse: true, bandwidth: 0 }
    }

    #[test]
    fn encodes_iperf3_field_names() {
        let j = encode(&sample());
        assert_eq!(j["tcp"], serde_json::json!(true));
        assert_eq!(j["time"], serde_json::json!(10));
        assert_eq!(j["parallel"], serde_json::json!(2));
        assert_eq!(j["len"], serde_json::json!(131072));
        assert_eq!(j["reverse"], serde_json::json!(true));
        assert!(j.get("udp").is_none());
        assert!(j.get("bandwidth").is_none()); // tcp: no pacing field
        assert!(j.get("client_version").is_some());
    }

    #[test]
    fn encodes_udp_with_bandwidth_and_omits_reverse_when_false() {
        let p = TestParams { udp: true, reverse: false, bandwidth: 1048576, len: 1460, ..sample() };
        let j = encode(&p);
        assert_eq!(j["udp"], serde_json::json!(true));
        assert_eq!(j["bandwidth"], serde_json::json!(1048576));
        assert_eq!(j["len"], serde_json::json!(1460));
        assert!(j.get("tcp").is_none());
        assert!(j.get("reverse").is_none());
    }

    #[test]
    fn decodes_its_own_output_and_tolerates_unknown_fields() {
        let mut j = encode(&sample());
        j["MSS"] = serde_json::json!(1400);
        j["congestion"] = serde_json::json!("cubic");
        assert_eq!(decode(j).unwrap(), sample());
    }

    #[test]
    fn round_trips_udp_params() {
        let p = TestParams { udp: true, time: 10, parallel: 2, len: 1460, reverse: false, bandwidth: 1048576 };
        assert_eq!(decode(encode(&p)).unwrap(), p);
    }

    #[test]
    fn rejects_out_of_bounds_and_ambiguous_values() {
        assert!(decode(serde_json::json!({"tcp": true, "time": 10, "parallel": 500, "len": 1000})).is_err());
        assert!(decode(serde_json::json!({"tcp": true, "time": 10, "parallel": 1, "len": 99999999})).is_err());
        assert!(decode(serde_json::json!({"time": 10, "parallel": 1, "len": 1000})).is_err()); // neither
        assert!(decode(serde_json::json!({"tcp": true, "udp": true, "time": 10, "parallel": 1, "len": 1000})).is_err()); // both
        assert!(decode(serde_json::json!({"tcp": true, "time": 999999, "parallel": 1, "len": 1000})).is_err()); // time
    }
}
```

`TestParams` needs `#[derive(Debug, Clone, PartialEq, Eq)]` for these assertions.

`netsu-rs/src/protocol/results.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_iperf3_field_names() {
        let r = EndResults {
            sender_has_retransmits: -1,
            streams: vec![StreamResult {
                id: 1, bytes: 5000, retransmits: -1, jitter: 0.002,
                errors: 3, packets: 100, start_time: 0.0, end_time: 10.01,
            }],
        };
        let j = encode(&r);
        assert_eq!(j["cpu_util_total"], serde_json::json!(0.0));
        assert_eq!(j["sender_has_retransmits"], serde_json::json!(-1));
        let s = &j["streams"][0];
        assert_eq!(s["id"], serde_json::json!(1));
        assert_eq!(s["bytes"], serde_json::json!(5000));
        assert_eq!(s["jitter"], serde_json::json!(0.002));
        assert_eq!(s["errors"], serde_json::json!(3));
        assert_eq!(s["packets"], serde_json::json!(100));
        assert_eq!(s["start_time"], serde_json::json!(0.0));
        assert_eq!(s["end_time"], serde_json::json!(10.01));
        assert_eq!(decode(j).unwrap(), r);
    }
}
```

`EndResults` / `StreamResult` need `#[derive(Debug, Clone, PartialEq)]`.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test protocol::params protocol::results
```
Expected: FAIL — modules do not exist.

- [ ] **Step 3: Implement**

Use `#[derive(Serialize, Deserialize)]` with `#[serde(rename = "...")]` for the snake_case wire names, `#[serde(skip_serializing_if = "...")]` for the conditional fields, and `#[serde(default)]` on optional incoming fields. Tolerating unknown fields is serde's default for structs — do **not** add `#[serde(deny_unknown_fields)]`.

`client_version` is `format!("netsu-rs-{}", crate::VERSION)`. (The TS one sends `netsu-0.2.0`; distinct prefixes make interop logs readable.)

Bounds enforcement belongs in `decode`, returning `NetsuError::Protocol` with a message naming the offending field.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): params and results codecs with iperf3 field names"
```

---

### Task 4: Stats — interval meter and UDP jitter/loss tracker

Pure math. The jitter expectations are hand-computed from RFC 1889 — **do not change the expected values to make a test pass; fix the implementation.**

**Reference:** `packages/netsu/src/stats.ts`. PROTOCOL.md "UDP specifics" for the jitter and loss rules.

**Files:**
- Create: `netsu-rs/src/stats.rs`
- Modify: `netsu-rs/src/lib.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub fn bits_per_second(bytes: u64, seconds: f64) -> f64` (0.0 when seconds <= 0.0)
  - `pub struct IntervalReport { pub start: f64, pub end: f64, pub bytes: u64, pub bits_per_second: f64 }` (start/end in seconds since test start)
  - `pub struct IntervalMeter` with `pub fn new(start: Instant) -> Self`, `pub fn add(&mut self, bytes: u64)`, `pub fn snap(&mut self, now: Instant) -> IntervalReport`, `pub fn total_bytes(&self) -> u64`
  - `pub struct JitterTracker` with `pub fn new() -> Self`, `pub fn on_packet(&mut self, pcount: u32, sent_micros: u64, now_micros: u64)`, and getters `jitter_secs() -> f64`, `lost() -> u64`, `out_of_order() -> u64`, `received() -> u64`, `max_seq() -> u32`

Note the unit choice: the Rust tracker exposes `jitter_secs()` directly, because seconds is what goes on the wire. The TS one stores milliseconds and converts at the boundary, which was a standing footgun. Keep the internal accumulator in whatever unit you compute in, but expose seconds.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/src/stats.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bytes_over_seconds_to_bits_per_second() {
        assert_eq!(bits_per_second(1_000_000, 8.0), 1_000_000.0);
        assert_eq!(bits_per_second(100, 0.0), 0.0);
    }

    #[test]
    fn interval_meter_reports_deltas_and_running_total() {
        let t0 = Instant::now();
        let mut m = IntervalMeter::new(t0);
        m.add(500);
        m.add(500);
        let first = m.snap(t0 + Duration::from_secs(1));
        assert_eq!(first.bytes, 1000);
        assert!((first.start - 0.0).abs() < 1e-9);
        assert!((first.end - 1.0).abs() < 1e-9);
        assert!((first.bits_per_second - 8000.0).abs() < 1e-6);
        m.add(250);
        let second = m.snap(t0 + Duration::from_secs(2));
        assert!((second.start - 1.0).abs() < 1e-9);
        assert_eq!(second.bytes, 250);
        assert_eq!(m.total_bytes(), 1250);
    }

    #[test]
    fn tracks_loss_and_out_of_order_from_packet_counts() {
        let mut t = JitterTracker::new();
        t.on_packet(1, 0, 10_000);
        t.on_packet(2, 10_000, 20_000);
        t.on_packet(5, 40_000, 50_000); // 3,4 missing
        t.on_packet(4, 30_000, 55_000); // 4 arrives late
        assert_eq!(t.received(), 4);
        assert_eq!(t.max_seq(), 5);
        assert_eq!(t.out_of_order(), 1);
        assert_eq!(t.lost(), 1); // 5 expected, 4 received
    }

    #[test]
    fn computes_rfc1889_jitter_hand_computed_sequence() {
        // transit times (ms): 10, 12, 9  ->  d = 2, then 3
        // jitter = 0; then 0 + (2-0)/16 = 0.125; then 0.125 + (3-0.125)/16 = 0.3046875
        let mut t = JitterTracker::new();
        t.on_packet(1, 0, 10_000);
        t.on_packet(2, 100_000, 112_000);
        t.on_packet(3, 200_000, 209_000);
        let jitter_ms = t.jitter_secs() * 1000.0;
        assert!((jitter_ms - 0.3046875).abs() < 1e-4, "got {jitter_ms}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test stats
```
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement**

RFC 1889 recurrence, exactly as PROTOCOL.md states it:
`transit = arrival - sent; d = |transit - prev_transit|; jitter += (d - jitter) / 16`.
The first packet must not move jitter (there is no previous transit).

Loss: `lost = max_seq.saturating_sub(received)`. A packet whose `pcount <= max_seq` counts as out-of-order.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): interval meter and rfc1889 jitter/loss tracker"
```

---

### Task 5: TCP transport — BytePipe over TcpStream + data channel

**Reference:** `packages/netsu/src/transport/tcp.ts`, `packages/netsu/src/streams/channel.ts`. Read the guard comments in the TS file — each documents a bug found in review.

**Files:**
- Create: `netsu-rs/src/transport/mod.rs`, `netsu-rs/src/transport/tcp.rs`
- Create: `netsu-rs/src/streams/mod.rs`, `netsu-rs/src/streams/channel.rs`
- Modify: `netsu-rs/src/lib.rs`

**Interfaces:**
- Consumes: `BytePipe`, `Result`
- Produces:
  - `channel.rs`: `#[async_trait] pub trait DataChannel: Send { async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()>; async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize>; async fn close(&mut self); fn error(&self) -> Option<&NetsuError>; }`
  - `tcp.rs`: `pub struct TcpPipe` implementing `BytePipe`, with `pub async fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpPipe>`, `pub fn from_stream(s: TcpStream) -> TcpPipe`, and `pub fn into_data_channel(self) -> Result<TcpDataChannel>`; `pub struct TcpDataChannel` implementing `DataChannel`; `pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);`

Rust makes two of the TS implementation's four hard-won guards unnecessary by construction: `into_data_channel(self)` consumes the pipe, so "write after detach" and "close after detach" cannot compile. Keep the third: **`into_data_channel` must return an error if bytes are still buffered** — the protocol guarantees none, and silently discarding them would corrupt the stream. Document in a comment that the other two are enforced by the type system, so a reader comparing implementations understands why they are absent.

`connect` must honor its timeout — an unreachable host must not hang forever (the TS implementation shipped without this and it was flagged in review).

Backpressure comes free with tokio's `AsyncWriteExt::write_all`, which does not return until the data is accepted. No `drain` equivalent is needed.

- [ ] **Step 1: Write the failing test**

`netsu-rs/tests/tcp_transport.rs`:

```rust
use netsu::protocol::framing::{read_json, write_json, MAX_JSON};
use netsu::transport::tcp::{TcpPipe, CONNECT_TIMEOUT};
use tokio::net::TcpListener;

#[tokio::test]
async fn carries_framed_json_both_ways() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut pipe = TcpPipe::from_stream(sock);
        let msg: serde_json::Value = read_json(&mut pipe, MAX_JSON, None).await.unwrap();
        write_json(&mut pipe, &serde_json::json!({ "echo": msg })).await.unwrap();
    });

    let mut pipe = TcpPipe::connect("127.0.0.1", port, CONNECT_TIMEOUT).await.unwrap();
    write_json(&mut pipe, &serde_json::json!({ "hello": 1 })).await.unwrap();
    let got: serde_json::Value = read_json(&mut pipe, MAX_JSON, None).await.unwrap();
    assert_eq!(got, serde_json::json!({ "echo": { "hello": 1 } }));
    server.await.unwrap();
}

#[tokio::test]
async fn into_data_channel_moves_bulk_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut pipe = TcpPipe::from_stream(sock);
        pipe.read_exact(4, None).await.unwrap();          // handshake stand-in
        pipe.write_all(&[1]).await.unwrap();              // ack, gates the bulk write
        let mut ch = pipe.into_data_channel().unwrap();
        let mut buf = vec![0u8; 65536];
        let mut total = 0usize;
        while total < 65536 {
            match ch.read_chunk(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
        total
    });

    let mut pipe = TcpPipe::connect("127.0.0.1", port, CONNECT_TIMEOUT).await.unwrap();
    pipe.write_all(&[1, 2, 3, 4]).await.unwrap();
    pipe.read_exact(1, None).await.unwrap();              // wait for ack
    let mut ch = pipe.into_data_channel().unwrap();
    ch.write_chunk(&vec![7u8; 65536]).await.unwrap();
    ch.close().await;
    assert!(server.await.unwrap() >= 65536);
}

#[tokio::test]
async fn connect_times_out_rather_than_hanging() {
    // TEST-NET-1, reserved and non-routable: the handshake cannot complete.
    let start = std::time::Instant::now();
    let got = TcpPipe::connect("192.0.2.1", 5310, std::time::Duration::from_millis(300)).await;
    assert!(got.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}
```

If the sandboxed environment intercepts connections to reserved networks and the third test cannot fail as intended, report that rather than deleting the test — mark it `#[ignore]` with a comment naming the reason.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test tcp_transport
```
Expected: FAIL — modules do not exist.

- [ ] **Step 3: Implement**

Split the `TcpStream` with `tokio::io::split` if you need concurrent read and write; for the control channel a single owned stream is sufficient because the protocol is strictly turn-taking.

`TcpDataChannel::error()` returns a latched error. In Rust `write_chunk` returns `Result`, so an error surfaces immediately rather than being stranded — the latch exists so that a reader-side or teardown-time error is still visible during result finalization. Set `TCP_NODELAY` on both connect and accept.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): tcp transport - control pipe and data channel"
```

---

### Task 6: Client — full control state machine, verified against official iperf3

**Reference:** `packages/netsu/src/client.ts`, `packages/netsu/src/streams/runner.ts`. Read both fully; `client.ts`'s comments document the TEST_END ordering, the reverse-mode teardown race, and the EXCHANGE_RESULTS idempotence fix.

**Files:**
- Create: `netsu-rs/src/streams/runner.rs`, `netsu-rs/src/client.rs`
- Create: `netsu-rs/tests/common/mod.rs` (test helpers)
- Create: `netsu-rs/tests/client_iperf3.rs`
- Modify: `netsu-rs/src/lib.rs`

**Interfaces:**
- Consumes: everything from Tasks 2–5
- Produces:
  - `runner.rs`: `pub fn next_stream_id(existing_count: usize) -> u32` (the `1, 3, 4, 5, ...` sequence), `pub struct StreamCounters { pub id: u32, pub bytes: u64, pub packets: u64, pub jitter: f64, pub errors: u64 }`
  - `client.rs`: `pub struct ClientOptions { pub port: u16, pub transport: Transport, pub udp: bool, pub reverse: bool, pub duration: u32, pub parallel: u32, pub len: Option<usize>, pub bandwidth: Option<u64>, pub interval: Option<Duration> }` with `Default`; `pub enum Transport { Tcp, Ws }`; `pub struct TestResult { pub udp: bool, pub reverse: bool, pub duration_seconds: f64, pub sent_bytes: u64, pub received_bytes: u64, pub send_bits_per_second: f64, pub receive_bits_per_second: f64, pub local: EndResults, pub remote: EndResults, pub udp_stats: Option<UdpStats> }`; `pub struct UdpStats { pub jitter_secs: f64, pub lost: u64, pub packets: u64, pub lost_percent: f64 }`; `pub async fn run_client(host: &str, opts: ClientOptions, on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>) -> Result<TestResult>`
  - `tests/common/mod.rs`: `pub fn has_iperf3() -> bool`, `pub async fn spawn_iperf3_server(port: u16, extra: &[&str]) -> Result<Child>`, `pub fn next_port() -> u16`

UDP and WS branches return `NetsuError::Protocol("udp wired in a later task")` / `"ws wired in a later task"` — Tasks 7 and 8 replace exactly those lines.

`spawn_iperf3_server` must pass `--forceflush` and detect readiness from the "Server listening" stdout banner. Without `--forceflush`, iperf3 block-buffers stdout when piped and the banner never arrives — Phase 1 lost real time to this, then to an `lsof`-polling workaround that failed on images without `lsof`. Do not reinvent either mistake.

`next_port()` must be collision-free across concurrently running test binaries. Cargo runs each integration-test file as a separate process, so a per-process counter is not enough — seed from the process id, or use an atomic over a per-file base within 5310–5360.

- [ ] **Step 1: Write the test helpers**

`netsu-rs/tests/common/mod.rs`:

```rust
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
```

`kill_on_drop(true)` is what guarantees no orphaned iperf3 processes even when a test panics.

- [ ] **Step 2: Write the failing integration tests**

`netsu-rs/tests/client_iperf3.rs`:

```rust
mod common;

use common::{has_iperf3, next_port, spawn_iperf3_server};
use netsu::client::{run_client, ClientOptions};

#[tokio::test]
async fn upload_transfers_and_exchanges_results() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions { port, duration: 2, ..Default::default() }, None)
        .await
        .unwrap();

    assert!(!r.reverse);
    assert!(r.sent_bytes > 1_000_000, "sent {}", r.sent_bytes);
    assert!(r.received_bytes > 0);
    assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
    assert!(r.send_bits_per_second > 1_000_000.0);
    let _ = server.kill().await;
}

#[tokio::test]
async fn reverse_receives_from_server() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions { port, duration: 2, reverse: true, ..Default::default() }, None)
        .await
        .unwrap();

    assert!(r.received_bytes > 1_000_000, "received {}", r.received_bytes);
    assert_eq!(r.local.sender_has_retransmits, -1); // we are the receiver
    let _ = server.kill().await;
}

#[tokio::test]
async fn parallel_three_streams_with_per_stream_results() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions { port, duration: 2, parallel: 3, ..Default::default() }, None)
        .await
        .unwrap();

    assert_eq!(r.local.streams.len(), 3);
    assert_eq!(r.remote.streams.len(), 3);
    // The iperf3 id quirk: 1, 3, 4 — not 1, 2, 3.
    let ids: Vec<u32> = r.local.streams.iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![1, 3, 4]);
    for s in &r.local.streams {
        assert!(s.bytes > 0);
    }
    let _ = server.kill().await;
}

#[tokio::test]
async fn reports_intervals_roughly_every_second() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = reports.clone();
    run_client(
        "127.0.0.1",
        ClientOptions { port, duration: 3, ..Default::default() },
        Some(Box::new(move |rep| sink.lock().unwrap().push(rep.bits_per_second))),
    )
    .await
    .unwrap();

    let got = reports.lock().unwrap();
    assert!(got.len() >= 2, "got {} interval reports", got.len());
    for bps in got.iter() {
        assert!(*bps > 0.0);
    }
    let _ = server.kill().await;
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test --test client_iperf3
```
Expected: FAIL — `netsu::client` does not exist.

- [ ] **Step 4: Implement**

Follow PROTOCOL.md's lifecycle diagram. The control loop reads a state byte and dispatches, exactly as `client.ts`'s `for(;;)` does. The pieces that matter:

- Send the 37-byte cookie immediately after connecting the control channel, and again as the first write on each TCP data stream.
- `IPERF_START` and `TEST_START` are informational — ignore them and keep looping.
- On `CREATE_STREAMS`, open `parallel` data connections, assigning ids via `next_stream_id`.
- On `TEST_RUNNING`, start the senders (or attach receivers when reverse), arm the duration timer and the interval timer.
- When the duration timer fires: **write `TEST_END` first, then stop the streams.** In reverse mode do not close the receive streams here at all — leave them to final cleanup, or you RST the server mid-write.
- On `EXCHANGE_RESULTS`: write your own results, then read the peer's. Make this path idempotent with the duration timer (set the end instant if unset; cancel the timer) so a server-driven early EXCHANGE_RESULTS cannot produce a negative `end_time` or a stray later `TEST_END` byte.
- On `DISPLAY_RESULTS`: reply `IPERF_DONE` and return the result.
- `ACCESS_DENIED` maps to `NetsuError::ServerBusy`, `SERVER_ERROR` to `NetsuError::ServerError`.
- `sender_has_retransmits` is `0` when sending, `-1` when receiving; per-stream `retransmits` is always `-1` (no TCP_INFO plumbing in this phase, matching the TS implementation and PROTOCOL.md's note).

The sender loop sends a fixed random chunk repeatedly. In tokio this is a plain `loop { ... }` with an `await` on the write, which yields to the runtime naturally — the TS implementation needed an explicit yield because its write could resolve synchronously, and Rust does not have that problem. Use `tokio::select!` against a shutdown signal rather than polling a flag.

Use one teardown path: a single function that aborts tasks, closes channels, and closes the control pipe, called on every exit including errors.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
pgrep iperf3 || echo "no orphans"
```
Expected: all pass, no orphaned iperf3.

- [ ] **Step 6: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): client control state machine, verified against official iperf3"
```

---

### Task 7: Server — accept rule, single-test lock, verified against official iperf3 client

**Reference:** `packages/netsu/src/server.ts`. PROTOCOL.md's connection-acceptance rule and "Error behavior".

**Files:**
- Create: `netsu-rs/src/server.rs`
- Create: `netsu-rs/tests/server_iperf3.rs`, `netsu-rs/tests/rs_to_rs.rs`
- Modify: `netsu-rs/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 2–6
- Produces: `pub struct ServerOptions { pub port: u16, pub transport: Transport, pub max_test_seconds: u32 }` with `Default` (`max_test_seconds: 3600`); `pub struct NetsuServer { pub port: u16 }` with `pub async fn close(self)`; `pub async fn start_server(opts: ServerOptions) -> Result<NetsuServer>`; and an internal `ServerCore::handle_connection` kept transport-agnostic so Task 9 reuses it for WS.

The accept rule (PROTOCOL.md): read 37 bytes; if no test is active it is a new control connection; if a test is active and the cookie matches during CREATE_STREAMS it is a data stream; otherwise reply `ACCESS_DENIED` and close.

Requirements that Phase 1's review caught the hard way:
- The single-test lock must be released on **every** exit path including errors and abort, or the server accepts one test and then refuses forever.
- `close()` must not hang on a connection that has not yet claimed a session (one sitting in the 37-byte cookie read). Track accepted connections and drop them on close.
- Cap the accepted `time` at `max_test_seconds` — otherwise a peer sending `{"time": 86400}` holds the exclusive lock for a day.
- Do not swallow the failure reason. Log it; the peer only ever sees `SERVER_ERROR`, so an opaque server is undiagnosable.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/tests/server_iperf3.rs`:

```rust
mod common;

use common::{has_iperf3, next_port};
use netsu::server::{start_server, ServerOptions};
use std::process::Stdio;
use tokio::process::Command;

async fn run_iperf3_client(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new("iperf3")
        .args(args)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn iperf3");
    let json = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("iperf3 output not json: {e}\n{}", String::from_utf8_lossy(&out.stdout)));
    (out.status.code().unwrap_or(-1), json)
}

#[tokio::test]
async fn iperf3_upload_completes_and_reports_bytes() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let (code, json) = run_iperf3_client(&["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2"]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum_sent"]["bytes"].as_u64().unwrap() > 1_000_000);
    assert!(json["end"]["sum_received"]["bytes"].as_u64().unwrap() > 0);

    server.close().await;
}

#[tokio::test]
async fn iperf3_reverse_receives_from_netsu_server() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let (code, json) = run_iperf3_client(&["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-R"]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum_received"]["bytes"].as_u64().unwrap() > 1_000_000);

    server.close().await;
}

#[tokio::test]
async fn iperf3_parallel_two_streams() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let (code, json) = run_iperf3_client(&["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-P", "2"]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");

    server.close().await;
}
```

`netsu-rs/tests/rs_to_rs.rs`:

```rust
mod common;

use common::next_port;
use netsu::client::{run_client, ClientOptions};
use netsu::server::{start_server, ServerOptions};

#[tokio::test]
async fn tcp_matrix_reverse_and_parallel() {
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            let port = next_port();
            let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

            let r = run_client(
                "127.0.0.1",
                ClientOptions { port, duration: 1, reverse, parallel, ..Default::default() },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("reverse={reverse} parallel={parallel}: {e}"));

            assert!(r.sent_bytes > 100_000, "reverse={reverse} parallel={parallel}");
            assert!(r.received_bytes > 0);
            assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
            assert_eq!(r.local.streams.len(), parallel as usize);

            server.close().await;
        }
    }
}

#[tokio::test]
async fn serves_a_second_test_after_the_first_finishes() {
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    run_client("127.0.0.1", ClientOptions { port, duration: 1, ..Default::default() }, None).await.unwrap();
    let again = run_client("127.0.0.1", ClientOptions { port, duration: 1, ..Default::default() }, None).await.unwrap();
    assert!(again.sent_bytes > 0);

    server.close().await;
}

#[tokio::test]
async fn rejects_a_concurrent_client_with_access_denied() {
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let first = tokio::spawn(run_client("127.0.0.1", ClientOptions { port, duration: 2, ..Default::default() }, None));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let second = run_client("127.0.0.1", ClientOptions { port, duration: 1, ..Default::default() }, None).await;
    assert!(matches!(second, Err(netsu::error::NetsuError::ServerBusy)));

    first.await.unwrap().unwrap();
    server.close().await;
}

#[tokio::test]
async fn rejects_a_requested_time_over_the_server_cap() {
    let port = next_port();
    let server = start_server(ServerOptions { port, max_test_seconds: 5, ..Default::default() }).await.unwrap();

    let got = run_client("127.0.0.1", ClientOptions { port, duration: 60, ..Default::default() }, None).await;
    assert!(got.is_err());

    server.close().await;
}

#[tokio::test]
async fn malformed_control_input_does_not_wedge_the_server() {
    use tokio::io::AsyncWriteExt;
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    // Connect, send a valid-looking cookie, then garbage where params JSON belongs.
    {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        sock.write_all(&[b'a'; 37]).await.unwrap();
        sock.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).await.unwrap(); // absurd length prefix
        sock.shutdown().await.ok();
    }

    // The server must return to idle and serve a real test.
    let r = run_client("127.0.0.1", ClientOptions { port, duration: 1, ..Default::default() }, None).await.unwrap();
    assert!(r.sent_bytes > 0);

    server.close().await;
}
```

That last test closes a gap the TypeScript suite still has — PROTOCOL.md specifies the behavior but nothing exercised it.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test server_iperf3 --test rs_to_rs
```
Expected: FAIL — `netsu::server` does not exist.

- [ ] **Step 3: Implement**

Structure: an accept loop spawning a task per connection, a `ServerCore` holding `Option<ActiveSession>` behind a `tokio::sync::Mutex`, and a `ServerSession` running the mirror of the client's state machine. Derive stream ids with `next_stream_id`, same as the client. The server sends its results *after* reading the client's.

Reverse mode means the server is the sender; forward mode it is a pure receiver.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
pgrep iperf3 || echo "no orphans"
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): server with iperf3 accept rule and single-test lock"
```

---

### Task 8: UDP — connect handshake, packet format, pacing, jitter/loss

**Reference:** `packages/netsu/src/transport/udp.ts` — read it closely, especially the byte-order derivation comment and the send-capability probe. PROTOCOL.md "UDP specifics".

**Files:**
- Create: `netsu-rs/src/transport/udp.rs`
- Modify: `netsu-rs/src/client.rs`, `netsu-rs/src/server.rs` (replace the UDP placeholder errors)
- Create: `netsu-rs/tests/udp_interop.rs`

**Interfaces:**
- Produces:
  - `pub const UDP_CONNECT_MSG: [u8; 4] = [0x39, 0x38, 0x37, 0x36];` (ASCII `"9876"`)
  - `pub const UDP_CONNECT_REPLY: [u8; 4] = [0x36, 0x37, 0x38, 0x39];` (ASCII `"6789"`)
  - `pub const LEGACY_UDP_CONNECT_REPLY: [u8; 4] = [0xB1, 0x68, 0xDE, 0x3A];`
  - `pub fn write_udp_header(buf: &mut [u8], pcount: u32, now_micros: u64)` — `sec(u32 BE) | usec(u32 BE) | pcount(u32 BE)` at offset 0
  - `pub fn read_udp_header(buf: &[u8]) -> Result<(u32, u64)>` returning `(pcount, sent_micros)`
  - `pub const UDP_HEADER_SIZE: usize = 12;`
  - `pub struct Pacer` with `pub fn new(bits_per_second: u64) -> Self` and `pub async fn gate(&mut self, bits: u64)`
  - `pub async fn probe_max_udp_send_len(requested: usize) -> usize` — the real maximum emittable datagram, determined by an actual send on a private loopback socket
  - `pub async fn udp_client_connect(host: &str, port: u16) -> Result<UdpSocket>` — hello sent, reply received (accepts legacy), 5s timeout
  - `pub async fn udp_server_bind(port: u16) -> Result<UdpSocket>` and `pub async fn udp_server_accept(sock: UdpSocket, timeout: Duration) -> Result<UdpSocket>`

**These constants are raw wire bytes.** Declaring them as `[u8; 4]` rather than `u32` makes the endianness bug from Phase 1 unrepresentable. Do not "simplify" them back into integers.

**Ordering that is load-bearing:** the first UDP bind must happen **before** the server announces CREATE_STREAMS. Official iperf3 clients send their hello exactly once with no retry, so a late bind silently loses it and the test hangs. Per stream the sequence is bind → accept → `connect()` to the peer (pins the 4-tuple) → bind a NEW socket with SO_REUSEADDR on the same port for the next stream → reply. Bind the next listener *before* sending the reply, closing the window where a fast client's next hello finds no listener.

**Block size:** clamp the sender's chunk to `probe_max_udp_send_len`. If nothing is sendable, refuse at PARAM_EXCHANGE with a diagnostic rather than proceeding at an untested size.

**Error policy:** a UDP send error increments `errors` and the loop continues. Credit `bytes` only on success. Keep `pcount` advancing per attempt.

**Pacing:** a real token bucket with a capped burst, defaulting to 1 Mbit/s. The `gate` must always yield to the runtime, even when it decides no sleep is needed — Phase 1's TypeScript version returned early on the unpaced path and livelocked the process at 99% CPU, unresponsive to SIGTERM, reachable by any peer sending `iperf3 -u -b 0 -R`. In tokio, `await`ing a zero sleep or `tokio::task::yield_now()` on that branch is sufficient.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/src/transport/udp.rs` unit tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_constants_are_the_documented_wire_bytes() {
        assert_eq!(&UDP_CONNECT_MSG, b"9876");
        assert_eq!(&UDP_CONNECT_REPLY, b"6789");
    }

    #[test]
    fn packet_header_round_trips_pcount_and_timestamp() {
        let mut buf = vec![0u8; 64];
        write_udp_header(&mut buf, 42, 1_500_000_123_456);
        let (pcount, sent) = read_udp_header(&buf).unwrap();
        assert_eq!(pcount, 42);
        // Second resolution on sec + microsecond remainder.
        assert_eq!(sent / 1_000_000, 1_500_000_123_456 / 1_000_000);
    }

    #[tokio::test]
    async fn pacer_holds_about_1mbit_per_second() {
        let mut p = Pacer::new(1_000_000);
        let start = std::time::Instant::now();
        for _ in 0..25 {
            p.gate(5000).await; // 25 x 5000 bits = 125_000 bits = 0.125s at 1Mbit
        }
        assert!(start.elapsed() >= std::time::Duration::from_millis(90), "elapsed {:?}", start.elapsed());
    }

    #[tokio::test]
    async fn unpaced_gate_still_yields() {
        // rate 0 means unpaced; it must not spin. If gate() never yields, this
        // test deadlocks the runtime rather than completing.
        let mut p = Pacer::new(0);
        for _ in 0..10_000 {
            p.gate(12_000).await;
        }
    }
}
```

`netsu-rs/tests/udp_interop.rs`:

```rust
mod common;

use common::{has_iperf3, next_port, spawn_iperf3_server};
use netsu::client::{run_client, ClientOptions};
use netsu::server::{start_server, ServerOptions};
use std::process::Stdio;
use tokio::process::Command;

async fn run_iperf3_client(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new("iperf3").args(args).arg("--json")
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output().await.expect("spawn iperf3");
    let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("iperf3 output not json: {e}\n{}", String::from_utf8_lossy(&out.stdout))
    });
    (out.status.code().unwrap_or(-1), json)
}

#[tokio::test]
async fn netsu_client_to_iperf3_server_udp() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions {
        port, duration: 2, udp: true, bandwidth: Some(5_000_000), ..Default::default()
    }, None).await.unwrap();

    let u = r.udp_stats.expect("udp stats");
    assert!(u.packets > 100, "packets {}", u.packets);
    assert!(u.lost_percent < 10.0, "lost {}%", u.lost_percent);
    let _ = server.kill().await;
}

#[tokio::test]
async fn netsu_client_reverse_from_iperf3_server_udp() {
    // The gap the TS suite still has: netsu as UDP *receiver* from official iperf3.
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions {
        port, duration: 2, udp: true, reverse: true, bandwidth: Some(5_000_000), ..Default::default()
    }, None).await.unwrap();

    let u = r.udp_stats.expect("udp stats");
    assert!(u.packets > 100, "packets {}", u.packets);
    assert!(u.lost_percent < 10.0, "lost {}%", u.lost_percent);
    let _ = server.kill().await;
}

#[tokio::test]
async fn iperf3_client_to_netsu_server_udp() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let (code, json) = run_iperf3_client(&[
        "-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-u", "-b", "5M", "-l", "1460",
    ]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["packets"].as_u64().unwrap() > 100);
    assert!(json["end"]["sum"]["lost_percent"].as_f64().unwrap() < 10.0);

    server.close().await;
}

#[tokio::test]
async fn iperf3_reverse_client_to_netsu_server_udp_unpinned_blocksize() {
    // No -l: iperf3 negotiates blksize from path MTU (16332 on loopback).
    // This is the case that exposed the send-capability bug in Phase 1.
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let (code, json) = run_iperf3_client(&[
        "-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-u", "-b", "5M", "-R",
    ]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["bytes"].as_u64().unwrap() > 100_000);
    assert!(json["end"]["sum"]["lost_percent"].as_f64().unwrap() < 10.0);

    server.close().await;
}

#[tokio::test]
async fn iperf3_parallel_udp_streams_to_netsu_server() {
    if !has_iperf3() { eprintln!("skipping: no iperf3"); return; }
    let port = next_port();
    let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

    let (code, json) = run_iperf3_client(&[
        "-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-u", "-b", "5M", "-l", "1460", "-P", "4",
    ]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");

    server.close().await;
}

#[tokio::test]
async fn udp_rs_to_rs_matrix() {
    // Includes parallel, the coverage the TS suite lacks netsu-to-netsu.
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            let port = next_port();
            let server = start_server(ServerOptions { port, ..Default::default() }).await.unwrap();

            let r = run_client("127.0.0.1", ClientOptions {
                port, duration: 1, udp: true, reverse, parallel,
                bandwidth: Some(5_000_000), ..Default::default()
            }, None).await.unwrap_or_else(|e| panic!("reverse={reverse} parallel={parallel}: {e}"));

            let u = r.udp_stats.expect("udp stats");
            assert!(u.packets > 0);
            assert!(u.lost_percent < 10.0, "reverse={reverse} parallel={parallel} lost {}%", u.lost_percent);
            assert_eq!(r.local.streams.len(), parallel as usize);

            server.close().await;
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test udp_interop
```
Expected: FAIL — UDP paths return the placeholder error.

- [ ] **Step 3: Implement**

Wire `udp.rs` into the client's `open_stream` and the server's session, replacing the placeholder errors.

Receiver side: on each datagram, parse the header, feed `JitterTracker::on_packet`, and accumulate bytes. Report `packets` on the wire as the max pcount seen (received + lost), matching iperf3's receiver semantics.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
pgrep iperf3 || echo "no orphans"
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): udp streams - connect handshake, pacing, jitter and loss"
```

---

### Task 9: WebSocket transport — same state machine over WS frames

**Reference:** `packages/netsu/src/transport/ws.ts`. PROTOCOL.md "WebSocket mode".

**Files:**
- Create: `netsu-rs/src/transport/ws.rs`
- Modify: `netsu-rs/src/client.rs`, `netsu-rs/src/server.rs` (replace the WS placeholder errors)
- Create: `netsu-rs/tests/ws.rs`

**Interfaces:**
- Produces: `pub struct WsPipe` implementing `BytePipe`, with `pub async fn connect(host: &str, port: u16, handshake_timeout: Duration) -> Result<WsPipe>` and `pub fn into_data_channel(self) -> Result<WsDataChannel>`; `pub struct WsDataChannel` implementing `DataChannel`

The design premise: **WS binary frames are a byte pipe.** The byte sequence on a WS channel is byte-for-byte identical to the TCP one — cookie, state bytes, length-prefixed JSON, payload. No extra framing, no per-message header, binary frames only. Control and each data stream get their own WS connection to `ws://host:port/`.

**Fragmentation across WS messages is arbitrary and the receiver must reassemble.** A `read_exact(37)` may span several messages, and one message may carry the tail of one unit plus the head of the next. Tests that only send conveniently-aligned messages will not catch a reassembly bug — the tests below deliberately do not.

`connect` must have a handshake timeout: a peer that completes the TCP handshake but never answers the HTTP upgrade would otherwise hang forever (this was a real finding in Phase 1).

A netsu server runs in either tcp mode or ws mode, never both on one port. Official iperf3 cannot connect to a ws-mode server; that is expected.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/tests/ws.rs`:

```rust
mod common;

use common::next_port;
use netsu::client::{run_client, ClientOptions, Transport};
use netsu::server::{start_server, ServerOptions};

#[tokio::test]
async fn ws_matrix_reverse_and_parallel() {
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            let port = next_port();
            let server = start_server(ServerOptions {
                port, transport: Transport::Ws, ..Default::default()
            }).await.unwrap();

            let r = run_client("127.0.0.1", ClientOptions {
                port, transport: Transport::Ws, duration: 1, reverse, parallel, ..Default::default()
            }, None).await.unwrap_or_else(|e| panic!("reverse={reverse} parallel={parallel}: {e}"));

            assert!(r.sent_bytes > 100_000, "reverse={reverse} parallel={parallel}");
            assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
            assert_eq!(r.local.streams.len(), parallel as usize);

            server.close().await;
        }
    }
}

#[tokio::test]
async fn ws_server_enforces_the_single_test_lock() {
    let port = next_port();
    let server = start_server(ServerOptions { port, transport: Transport::Ws, ..Default::default() }).await.unwrap();

    let first = tokio::spawn(run_client("127.0.0.1", ClientOptions {
        port, transport: Transport::Ws, duration: 2, ..Default::default()
    }, None));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let second = run_client("127.0.0.1", ClientOptions {
        port, transport: Transport::Ws, duration: 1, ..Default::default()
    }, None).await;
    assert!(second.is_err());

    first.await.unwrap().unwrap();
    server.close().await;
}

#[tokio::test]
async fn ws_connect_times_out_against_a_non_upgrading_peer() {
    // A plain TCP listener that accepts but never answers the HTTP upgrade.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        // Hold the connection open, answer nothing.
        std::mem::forget(sock);
    });

    let start = std::time::Instant::now();
    let got = netsu::transport::ws::WsPipe::connect("127.0.0.1", port, std::time::Duration::from_millis(500)).await;
    assert!(got.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}
```

`netsu-rs/tests/ws_reassembly.rs`:

```rust
use futures_util::SinkExt;
use netsu::protocol::pipe::BytePipe;
use netsu::transport::ws::WsPipe;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Serve one WS connection, sending the caller-supplied chunks as separate
/// binary messages with a gap between them so each lands as its own frame.
async fn serve_chunks(chunks: Vec<Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        for c in chunks {
            ws.send(Message::Binary(c)).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    });
    port
}

#[tokio::test]
async fn reassembles_one_protocol_unit_split_across_many_ws_messages() {
    let unit: Vec<u8> = (0..37u8).collect();
    // Deliberately unaligned split: 3, 1, 20, 6, 7
    let chunks = vec![
        unit[0..3].to_vec(), unit[3..4].to_vec(), unit[4..24].to_vec(),
        unit[24..30].to_vec(), unit[30..37].to_vec(),
    ];
    let port = serve_chunks(chunks).await;

    let mut pipe = WsPipe::connect("127.0.0.1", port, std::time::Duration::from_secs(5)).await.unwrap();
    assert_eq!(pipe.read_exact(37, None).await.unwrap(), unit);
}

#[tokio::test]
async fn handles_a_message_carrying_the_tail_of_one_unit_and_the_head_of_the_next() {
    let a: Vec<u8> = vec![0xAA; 37];
    let b: Vec<u8> = vec![0xBB; 37];
    // First message: all of A's head. Second: A's tail + B's head. Third: B's tail.
    let chunks = vec![
        a[0..30].to_vec(),
        [&a[30..37], &b[0..10]].concat(),
        b[10..37].to_vec(),
    ];
    let port = serve_chunks(chunks).await;

    let mut pipe = WsPipe::connect("127.0.0.1", port, std::time::Duration::from_secs(5)).await.unwrap();
    assert_eq!(pipe.read_exact(37, None).await.unwrap(), a);
    assert_eq!(pipe.read_exact(37, None).await.unwrap(), b);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test ws --test ws_reassembly
```
Expected: FAIL — WS paths return the placeholder error.

- [ ] **Step 3: Implement**

Use `tokio-tungstenite`. Keep a leftover-bytes buffer so `read_exact` can span messages — the same shape as `MemoryPipe`'s buffer. Send only `Message::Binary`. Reuse `ServerCore::handle_connection` for the WS server rather than writing a second state machine.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): websocket transport - protocol tunneled over ws frames"
```

---

### Task 10: CLI — clap, iperf3-style flags, interval lines, --json

**Reference:** `packages/netsu/src/cli.ts`, `packages/netsu/src/format.ts`. Match the TS CLI's flag surface and output shape so the two implementations are drop-in comparable in the Phase 3 interop matrix.

**Files:**
- Create: `netsu-rs/src/format.rs`
- Modify: `netsu-rs/src/main.rs`
- Create: `netsu-rs/tests/cli.rs`

**Interfaces:**
- Produces: a `netsu` binary with:

```
netsu server [-p 5201] [--ws]
netsu client <host> [-p 5201] [-u | --ws] [-t 10] [-P 1] [-R] [-b 1M] [-l 128K] [-i 1] [--json]
```

- `format.rs`: `pub fn parse_bandwidth(s: &str) -> Result<u64>` (decimal K/M/G — `1M` is 1_000_000, **not** 1_048_576; verified against real iperf3), `pub fn parse_len(s: &str) -> Result<usize>` (binary K/M — a byte count, so `128K` is 131_072), `pub fn format_bits(v: f64) -> String`, `pub fn format_bytes(v: u64) -> String`, `pub fn interval_line(r: &IntervalReport) -> String`

Requirements, all of which the TS CLI review found the hard way:
- A failed test exits non-zero. Printing an error and exiting 0 breaks every wrapping script.
- Under `--json`, **stdout carries nothing but JSON** — no interval lines, no banners. Diagnostics go to stderr. This must hold on the failure path too, and for clap's own usage errors.
- Errors are legible: surface `ServerBusy` as "server busy", not a debug-formatted enum or a raw io error.
- Ctrl-C (SIGINT) and SIGTERM terminate cleanly and release the port. Use `tokio::signal`.
- Argument validation rejects `-P 0`, `-t 0`, and an unparseable `-b` with a clear message and non-zero exit, before any network I/O.
- `-b` and `-l` use *different* multiplier bases, as above. This is iperf3's convention, not an inconsistency to unify.

- [ ] **Step 1: Write the failing tests**

`netsu-rs/src/format.rs` unit tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_suffixes_are_decimal_matching_iperf3() {
        assert_eq!(parse_bandwidth("1000").unwrap(), 1_000);
        assert_eq!(parse_bandwidth("10K").unwrap(), 10_000);
        assert_eq!(parse_bandwidth("1M").unwrap(), 1_000_000);
        assert_eq!(parse_bandwidth("1G").unwrap(), 1_000_000_000);
        assert_eq!(parse_bandwidth("0").unwrap(), 0); // iperf3's "unlimited"
        assert!(parse_bandwidth("fast").is_err());
    }

    #[test]
    fn len_suffixes_are_binary_because_it_is_a_byte_count() {
        assert_eq!(parse_len("1460").unwrap(), 1460);
        assert_eq!(parse_len("128K").unwrap(), 131_072);
        assert_eq!(parse_len("1M").unwrap(), 1_048_576);
        assert!(parse_len("big").is_err());
    }
}
```

`netsu-rs/tests/cli.rs`:

```rust
mod common;

use common::next_port;
use std::process::Stdio;
use tokio::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_netsu")
}

#[tokio::test]
async fn server_and_client_run_a_tcp_test_end_to_end() {
    let port = next_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true).spawn().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let out = Command::new(bin())
        .args(["client", "127.0.0.1", "-p", &port.to_string(), "-t", "1"])
        .output().await.unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("bits/sec"));
    let _ = server.kill().await;
}

#[tokio::test]
async fn json_mode_emits_only_json_on_stdout() {
    let port = next_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true).spawn().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let out = Command::new(bin())
        .args(["client", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-i", "1", "--json"])
        .output().await.unwrap();

    assert!(out.status.success());
    assert!(out.stderr.is_empty(), "stderr not empty: {}", String::from_utf8_lossy(&out.stderr));
    let _: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout must be pure json");
    let _ = server.kill().await;
}

#[tokio::test]
async fn connection_refused_exits_nonzero_with_empty_stdout_under_json() {
    let port = next_port(); // nothing listening
    let out = Command::new(bin())
        .args(["client", "127.0.0.1", "-p", &port.to_string(), "-t", "1", "--json"])
        .output().await.unwrap();

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
    assert!(!out.stderr.is_empty());
}

#[tokio::test]
async fn argument_validation_rejects_bad_flags_before_network_io() {
    for args in [
        vec!["client", "127.0.0.1", "-P", "0"],
        vec!["client", "127.0.0.1", "-t", "0"],
        vec!["client", "127.0.0.1", "-b", "fast"],
    ] {
        let out = Command::new(bin()).args(&args).output().await.unwrap();
        assert!(!out.status.success(), "expected failure for {args:?}");
        assert!(out.stdout.is_empty(), "stdout not empty for {args:?}");
    }
}

#[tokio::test]
async fn sigint_during_an_active_test_frees_the_port() {
    let port = next_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true).spawn().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let pid = server.id().expect("pid") as i32;
    unsafe { libc::kill(pid, libc::SIGINT) };
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), server.wait())
        .await
        .expect("server must exit on SIGINT")
        .unwrap();
    let _ = status;

    // Port must be immediately rebindable.
    tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("port still held");
}
```

Add `libc = "0.2"` to `[dev-dependencies]` for the SIGINT test.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --test cli
```
Expected: FAIL — the binary has no subcommands.

- [ ] **Step 3: Implement**

Use clap's derive API. Keep the CLI thin: parsing, validation, output formatting, and process lifecycle only. All measurement logic already lives in the library — do not reimplement any of it.

For `--json`, build the output structure to match the TS CLI's shape (`start`, `intervals`, `end` with `sum_sent`/`sum_received`, and `end.sum` for UDP) so the Phase 3 matrix can parse both implementations with one script.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
cargo build --release && ./target/release/netsu --help
```

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "feat(rs): cli with iperf3-style flags, interval lines and --json"
```

---

### Task 11: README, crate metadata, and rs↔ts interop smoke test

Make the crate publishable and prove the two implementations actually talk to each other.

**Files:**
- Create: `netsu-rs/README.md`
- Modify: `netsu-rs/Cargo.toml` (metadata: keywords, categories, exclude)
- Create: `netsu-rs/tests/ts_interop.rs`

**Interfaces:**
- Consumes: everything
- Produces: `cargo publish --dry-run` passing; a test proving Rust ↔ TypeScript interop over TCP

- [ ] **Step 1: Write the failing interop test**

`netsu-rs/tests/ts_interop.rs`:

```rust
mod common;

use common::next_port;
use netsu::client::{run_client, ClientOptions};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

fn ts_package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../packages/netsu")
}

fn ts_cli_built() -> bool {
    ts_package_dir().join("dist/cli.mjs").exists()
}

/// Start the TypeScript netsu server via its built CLI.
async fn spawn_ts_server(port: u16) -> std::io::Result<Child> {
    let mut child = Command::new("node")
        .arg(ts_package_dir().join("dist/cli.mjs"))
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Ok(Some(l)) = lines.next_line().await {
            if l.contains("listening") { return true; }
        }
        false
    }).await.unwrap_or(false);
    if !ready {
        let _ = child.kill().await;
        return Err(std::io::Error::other("ts server did not start"));
    }
    Ok(child)
}

#[tokio::test]
async fn rust_client_against_typescript_server_tcp() {
    if !ts_cli_built() {
        eprintln!("skipping: packages/netsu/dist/cli.mjs not built (run `bun run build` there)");
        return;
    }
    let port = next_port();
    let mut server = spawn_ts_server(port).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions { port, duration: 2, ..Default::default() }, None)
        .await
        .unwrap();

    assert!(r.sent_bytes > 1_000_000);
    assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
    let _ = server.kill().await;
}

#[tokio::test]
async fn rust_client_reverse_against_typescript_server_tcp() {
    if !ts_cli_built() {
        eprintln!("skipping: packages/netsu/dist/cli.mjs not built");
        return;
    }
    let port = next_port();
    let mut server = spawn_ts_server(port).await.unwrap();

    let r = run_client("127.0.0.1", ClientOptions { port, duration: 2, reverse: true, ..Default::default() }, None)
        .await
        .unwrap();

    assert!(r.received_bytes > 1_000_000);
    let _ = server.kill().await;
}
```

This is a smoke test, not the full matrix — Phase 3's docker harness owns the exhaustive cross-implementation grid. Its value here is catching a protocol divergence immediately, in the same `cargo test` run that would introduce it.

- [ ] **Step 2: Run it**

```bash
cd /Users/hk/Dev/netsu/packages/netsu && bun run build
cd /Users/hk/Dev/netsu/netsu-rs && cargo test --test ts_interop
```
Expected: PASS. If it fails, that is a real protocol divergence between the two implementations — debug it as such and report which side deviates from PROTOCOL.md. Do not weaken the assertions.

- [ ] **Step 3: Write the README**

`netsu-rs/README.md` — cover: what netsu is; that it speaks iperf3's wire protocol and interoperates with the real binary; install (`cargo install netsu`); the CLI surface with a worked example of both `server` and `client`; the library API with a short `run_client` example; a pointer to `../PROTOCOL.md`; and a note that the TypeScript implementation lives in `packages/netsu`. Every command you write must be one you actually ran.

- [ ] **Step 4: Finish crate metadata and verify packaging**

Add to `Cargo.toml`:

```toml
keywords = ["iperf3", "network", "benchmark", "speedtest", "bandwidth"]
categories = ["command-line-utilities", "network-programming"]
exclude = ["tests/", "/target"]
```

```bash
cargo publish --dry-run
```
Expected: succeeds. Inspect the packaged file list and confirm nothing unexpected ships.

- [ ] **Step 5: Full verification**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo build --release
pgrep iperf3 || echo "no orphans"
```

- [ ] **Step 6: Commit**

```bash
cd /Users/hk/Dev/netsu
git add netsu-rs && git commit -m "docs(rs): readme, crate metadata, and ts interop smoke test"
```

---

## Self-Review Notes

**Spec coverage.** Design spec §5 requires the mirrored module layout (Tasks 2–10 follow `protocol/`, `transport/`, `client`, `server`, `streams/`, `stats`, `cli`), §4 the iperf3 protocol including the UDP handshake (Task 8) and WS tunneling (Task 9), §6 the CLI surface and library API (Tasks 6, 7, 10), §7 the error handling and input bounds (Tasks 3, 7), §8.1–8.2 unit and same-implementation integration tests (every task). §3's `cargo install` path is Task 11. Cross-compilation for e2e (§8.3) and the CI overhaul (§8.4) are Phase 3, deliberately not here.

**Deliberate divergences from the TS implementation**, each because Rust makes the better option cheap: `JitterTracker` exposes seconds rather than milliseconds (removing a unit-conversion footgun); `into_data_channel(self)` makes two of the TS detach guards unrepresentable rather than checked at runtime; the UDP handshake constants are `[u8; 4]` rather than integers, making the Phase 1 endianness bug impossible. Each is called out in its task so a reader comparing implementations knows the difference is intentional.

**Coverage this plan adds that the TS suite lacks**, from the Phase 1 final review: netsu-as-UDP-receiver against official iperf3 (Task 8), UDP parallel streams netsu-to-netsu (Task 8), and the server's malformed-input recovery path (Task 7).
