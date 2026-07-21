# iroh transport + multiplexing lab — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `iroh-mux-bench`'s capabilities into `netsu` — iroh as a 4th
transport, a `netsu mux` multiplexing/priority latency lab, rendez-key ticket
exchange, a ratatui TUI, and a kbm demo example — all behind optional cargo
features, while slimming the default binary.

**Architecture:** Three destinations (see spec §1): iroh throughput plugs into
netsu's existing iperf3 core as `Transport::Iroh`; the mux lab is a sibling
subsystem (`src/mux/`) with its own protocol; the kbm demo is an
`examples/` entry. A shared `src/p2p/` module holds iroh endpoint setup,
ticket + rendez-key addressing, and connection observation. A `tui` feature
adds a ratatui launcher + live dashboard over both throughput and mux runs.

**Tech Stack:** Rust 2024, tokio, iroh `=1.0.2` / iroh-tickets `=1.0.0`,
postcard, hdrhistogram, sysinfo, schemars, reqwest (rustls), ratatui 0.30.x +
crossterm, monio `=0.1.1` (demo). Design spec:
`docs/superpowers/specs/2026-07-21-iroh-transport-and-mux-lab-design.md`.

## Global Constraints

- Default build (`cargo build`) is the smallest iperf3-compatible TCP+UDP tool:
  **no** ws / iroh / tui / monio deps. Target base ≈ 1.2–1.6 MiB stripped.
- Every non-core transport/UI is opt-in: features `ws`, `iroh`, `tui`,
  `input-demo` (implies `iroh`). `iroh` is a single feature (transport + mux +
  rendez-key).
- `[profile.release]`: `opt-level = 3` (NOT `"z"`/`"s"` — this is a benchmark),
  `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.
- iperf3 wire compatibility is preserved for TCP/UDP; the iroh throughput
  transport reuses netsu's existing control state machine unchanged.
- The `mux` protocol is netsu-internal (not iperf3-interop). No secrets ever
  uploaded to rendez-key; the ticket is always a working fallback.
- macОS netem runs only inside Docker's Linux VM (documented limitation).
- TDD: failing test → minimal impl → green → commit. Frequent commits.

---

## Phase 1 — Slim the base (feature-gate WS + size profile)

Independent of all iroh work; touches existing code only. Deliverable: default
build has no WebSocket deps and a size-optimized release profile; `--features
ws` restores WS and its tests pass.

### Task 1.1: Make WS an optional `ws` feature

**Files:**
- Modify: `netsu-rs/Cargo.toml` (deps → optional; add `[features]`)
- Modify: `netsu-rs/src/transport/mod.rs:8` (`pub mod ws;` → cfg)
- Modify: `netsu-rs/src/client.rs` (imports :81, enum variant :95, dispatch
  :165 & :444, fns `run_ws` :700 / `open_ws_stream` :763)
- Modify: `netsu-rs/src/server.rs` (import :52, accept arm :153-178)
- Modify: `netsu-rs/src/main.rs` (`--ws` handling :104, :121, :142-143, :175)
- Test: `netsu-rs/tests/ws.rs`, `netsu-rs/tests/ws_reassembly.rs` (add
  `#![cfg(feature = "ws")]`)

**Interfaces:**
- Produces: `Transport` enum with `Ws` variant present only under
  `#[cfg(feature = "ws")]`; `Transport::Tcp` always present and `#[default]`.
- Consumes: nothing new.

- [ ] **Step 1: Gate the ws test files so the default `cargo test` compiles.**
  Prepend to both `tests/ws.rs` and `tests/ws_reassembly.rs`:
  ```rust
  #![cfg(feature = "ws")]
  ```

- [ ] **Step 2: Verify the default test build now excludes them.**
  Run: `cd netsu-rs && cargo build --tests 2>&1 | tail -5`
  Expected: still compiles today (ws deps still present); this step just proves
  the `#![cfg]` guard parses. (It will matter after Step 4.)

- [ ] **Step 3: Cargo.toml — make WS deps optional and declare features.**
  Change the two dep lines to `optional = true`:
  ```toml
  tokio-tungstenite = { version = "0.24", optional = true }
  futures-util = { version = "0.3", optional = true }
  ```
  Add above `[dependencies]` (or below it):
  ```toml
  [features]
  default = []
  ws = ["dep:tokio-tungstenite", "dep:futures-util"]
  ```

- [ ] **Step 4: Gate `pub mod ws;`** in `src/transport/mod.rs`:
  ```rust
  #[cfg(feature = "ws")]
  pub mod ws;
  ```

- [ ] **Step 5: Gate the WS bits in `client.rs`.**
  - Import (line ~81): `#[cfg(feature = "ws")] use crate::transport::ws::{WS_CONNECT_TIMEOUT, WsPipe};`
  - Enum variant (line ~95):
    ```rust
    pub enum Transport {
        #[default]
        Tcp,
        #[cfg(feature = "ws")]
        Ws,
    }
    ```
  - Dispatch (line ~165): put `#[cfg(feature = "ws")]` on the `Transport::Ws => run_ws(...)` arm.
  - Dispatch (line ~444): put `#[cfg(feature = "ws")]` on the `Transport::Ws => open_ws_stream(...)` arm.
  - Functions `run_ws` (~700) and `open_ws_stream` (~763): prefix each with `#[cfg(feature = "ws")]`.

- [ ] **Step 6: Gate the WS bits in `server.rs`.**
  - Import (line ~52): `#[cfg(feature = "ws")] use crate::transport::ws::WsPipe;`
  - Accept arm (lines ~153-178): put `#[cfg(feature = "ws")]` on the whole
    `Transport::Ws => { … }` match arm.

- [ ] **Step 7: `main.rs` — keep `--ws` parseable, error helpfully when absent.**
  Add a shared helper near the top of `main.rs`:
  ```rust
  /// Resolve the `--ws` flag against compiled features.
  fn ws_transport(ws: bool) -> Result<netsu::client::Transport, String> {
      use netsu::client::Transport;
      if ws {
          #[cfg(feature = "ws")]
          { Ok(Transport::Ws) }
          #[cfg(not(feature = "ws"))]
          { Err("ws support not compiled in; rebuild with --features ws".into()) }
      } else {
          Ok(Transport::Tcp)
      }
  }
  ```
  In `run_server` replace `let transport = if a.ws { Transport::Ws } else { Transport::Tcp };` with:
  ```rust
  let transport = match ws_transport(a.ws) { Ok(t) => t, Err(e) => { eprintln!("netsu server: {e}"); return 1; } };
  ```
  In `run_client_inner` replace `transport: if a.ws { Transport::Ws } else { Transport::Tcp },` by computing it first:
  ```rust
  let transport = ws_transport(a.ws).map_err(|e| e)?;   // a.udp && a.ws check stays above
  ```
  and use `transport,` in the `ClientOptions { … }`. The `if a.ws { "ws" } else { "tcp" }` label at ~121 stays as-is (still valid without the feature).

- [ ] **Step 8: Default build has no WS deps.**
  Run: `cd netsu-rs && cargo build 2>&1 | tail -3 && cargo tree -e no-dev 2>/dev/null | grep -c tungstenite`
  Expected: builds clean; grep count `0` (no tungstenite in the default tree).

- [ ] **Step 9: `--features ws` still builds and its tests pass.**
  Run: `cd netsu-rs && cargo test --features ws --test ws --test ws_reassembly 2>&1 | tail -8`
  Expected: ws tests PASS.

- [ ] **Step 10: Default test suite still green (non-ws).**
  Run: `cd netsu-rs && cargo test 2>&1 | tail -12`
  Expected: PASS (ws test files compiled out; TCP/UDP suites pass).

- [ ] **Step 11: Commit.**
  ```bash
  cd ~/Dev/netsu && git add netsu-rs/Cargo.toml netsu-rs/src/transport/mod.rs netsu-rs/src/client.rs netsu-rs/src/server.rs netsu-rs/src/main.rs netsu-rs/tests/ws.rs netsu-rs/tests/ws_reassembly.rs
  git commit -m "feat(rs): make the WebSocket transport an optional 'ws' feature"
  ```

### Task 1.2: Size-optimized release profile + measure the base

**Files:**
- Modify: `netsu-rs/Cargo.toml` (add `[profile.release]`)

- [ ] **Step 1: Add the profile.** Append to `netsu-rs/Cargo.toml`:
  ```toml
  [profile.release]
  opt-level = 3
  lto = true
  codegen-units = 1
  panic = "abort"
  strip = true
  ```

- [ ] **Step 2: Build the slim base and record its size.**
  Run: `cd netsu-rs && cargo build --release 2>&1 | tail -3 && ls -la target/release/netsu | awk '{print $5}'`
  Expected: builds; size materially below the prior 2,969,088 bytes (target
  ≈1.2–1.6 MiB). Record the number.

- [ ] **Step 3: TCP/UDP smoke still works with the slim release binary.**
  Run:
  ```bash
  cd netsu-rs && ./target/release/netsu server -p 5399 & SRV=$!; sleep 1
  ./target/release/netsu client 127.0.0.1 -p 5399 -t 2 --json | tail -c 200; echo
  kill $SRV 2>/dev/null
  ```
  Expected: a JSON result with non-zero `sum_sent`/`sum_received` bytes.

- [ ] **Step 4: Commit.**
  ```bash
  cd ~/Dev/netsu && git add netsu-rs/Cargo.toml
  git commit -m "build(rs): size-optimized release profile (LTO, strip, abort)"
  ```

---

## Phases 2–9 — task outlines (expanded to full TDD steps when reached)

Each phase below is a working, testable deliverable. Detailed bite-sized steps
are authored at the start of that phase (the exact iroh/ratatui APIs firm up
during implementation; writing exact code now would be speculative). Spec
section references in parentheses.

### Phase 2 — iroh throughput transport (spec §3, §4)
- **Files:** `Cargo.toml` (`iroh` feature + deps), `src/p2p/{mod,endpoint,observe}.rs`,
  `src/transport/iroh.rs` (BytePipe control + DataChannel data), `src/client.rs`
  (`Transport::Iroh` arm), `src/server.rs` (iroh accept path), `src/main.rs`
  (`--iroh` on server/client), `tests/iroh_transport.rs`.
- **Deliverable:** `netsu server --iroh` ↔ `netsu client <ticket> --iroh`
  measures throughput over one iroh connection; upload/reverse/parallel
  reconcile byte-exact; `--direct-only` fails on a relay path.
- **Tests:** in-process endpoint pair upload/reverse/parallel byte-exact;
  direct-path enforcement; default build (no iroh) errors `--iroh` cleanly.

### Phase 3 — rendez-key addressing (spec §5)
- **Files:** `src/p2p/{addr,rendezkey}.rs`, wire into `p2p` listener print +
  `--peer` resolution used by throughput (and later mux); `src/main.rs`
  (`--no-rendezkey`, `--rendezkey-url`, `--rendezkey-ttl/-reads`),
  `tests/rendezkey.rs`.
- **Deliverable:** listener prints `code` + `ticket`; `--peer <arg>` resolves a
  short code via claim or parses a long ticket directly (length discrimination,
  ≤16 → code). No-token/`--no-rendezkey` prints ticket only.
- **Tests:** `resolve_peer` length discrimination; claim happy path (mocked);
  fallback prints ticket; uniform-404 handling.

### Phase 4 — mux lab core (spec §6.1–6.4, §6.7)
- **Files:** `src/mux/{mod,config,workload,runner,receiver,metrics,protocol}.rs`,
  `src/main.rs` (`mux` subcommand: `listen`/`run`/`local`), `tests/mux_*.rs`.
- **Deliverable:** `netsu mux local` and `mux run`/`listen` run scenarios
  (4 presets + custom) over one iroh connection with per-stream QUIC priority,
  app-level pacing, and ACK-RTT latency measurement.
- **Tests:** one-connection invariant; graded priority order; dynamic priority
  change recorded; `cast_budget_matches_target_bitrate`; fixed-rate file
  interval; Pacer lateness w/o drift; custom-stream parse + probe/load class;
  frame round-trips + oversized/unknown-version rejection; failure paths.

### Phase 5 — mux output, results, schemas (spec §7)
- **Files:** `src/mux/{result,samples,resources,output}.rs`,
  `examples/write_mux_schema.rs`, `schema/mux-result-v1.json`,
  `schema/mux-samples-v1.ndjson`, `tests/mux_output.rs`.
- **Deliverable:** `--json-out`/`--samples-out`; schema-v1 result + NDJSON
  samples (drop-on-full); CPU/RSS via sysinfo.
- **Tests:** schema-v1 fields + checked-in schema match; one-JSON-line sample;
  hot-path emitter drops instead of blocking; atomic JSON replace;
  sent-vs-received reconciliation.

### Phase 6 — matrix + Docker/netem (spec §6.5, §6.6)
- **Files:** `src/mux/{matrix,netem}.rs`, `src/main.rs` (`mux matrix`),
  `mux-docker/{Dockerfile,docker-compose.yml,entrypoint.sh,netem-profiles.json}`,
  `scripts/mux-matrix.sh`, `scripts/mux-smoke.sh`, `tests/mux_matrix.rs`,
  `tests/netem_config.rs`.
- **Deliverable:** `mux matrix --profile required-v1` (20 cases) →
  `comparison.json` with `load_induced_input_p99_delta`; 5 netem profiles via
  tc/netem in Docker.
- **Tests:** required-v1 case count + aggregation identity; netem values reject
  shell metacharacters; entrypoint rejects injected rate (exit 64).

### Phase 7 — TUI (spec §9)
- **Files:** `Cargo.toml` (`tui` feature), `src/tui/{mod,app,forms,dashboard,live}.rs`,
  `src/main.rs` (`tui` subcommand), `LiveObserver` hooks on both runners,
  `tests/tui_forms.rs`, `tests/tui_dashboard.rs`.
- **Deliverable:** `netsu tui` — Home → config forms (incl. custom stream editor
  + equivalent-CLI line) → live dashboard (gauges, per-stream table, latency +
  sparkline) → summary; drives throughput and mux in-process.
- **Tests:** form-state → typed options (pure) + equivalent-CLI string;
  dashboard render from synthetic `LiveSnapshot` via ratatui `TestBackend`;
  iroh/mux screens hinted-absent without `--features iroh`.

### Phase 8 — kbm demo example (spec §8)
- **Files:** `Cargo.toml` (`input-demo` feature), `src/demo/**`,
  `examples/kbm-demo.rs` (`required-features = ["input-demo"]`),
  `tests/demo_*.rs`.
- **Deliverable:** `cargo run --example kbm-demo --features input-demo -- …`
  controller/controlled kbm sharing with bulk load; safety model intact.
- **Tests:** normalized-event round-trip; motion coalescing preserves
  transitions; stale/replayed rejection; bulk rate paces load; disconnect
  releases pressed input.

### Phase 9 — retire the old repo + docs (spec §11)
- **Files:** `netsu-rs/README.md`, root `PROTOCOL.md` (iroh binding + mux
  protocol sections), `docs/` (port the two empirical write-ups), `scripts/verify.sh`.
- **Deliverable:** docs updated; `verify.sh` gates fmt/clippy/`test --features
  ws,iroh,tui`/release smokes; `iroh-mux-bench` retired after green.
- **Tests:** `verify.sh` passes end to end.

---

## Self-review notes

- **Spec coverage:** every spec section (§1–§13) maps to a phase above
  (§2 features → Phase 1 + per-phase Cargo edits; §3–§4 → Phase 2; §5 → Phase 3;
  §6 → Phases 4/6; §7 → Phase 5; §8 → Phase 8; §9 → Phase 7; §10 tests → folded
  per phase; §11 → Phase 9; §12 risks → constraints; §13 phasing → this plan).
- **Ordering:** Phase 1 is independent and first. Phases 2→5 are linear;
  Phase 6 needs 4–5; Phase 7 needs 2–5 (matrix screen after 6); Phase 8 is
  near-independent (shares `Pacer`/`DeterministicBytes` from Phase 4); Phase 9
  last.
- **No placeholders in Phase 1** (the immediately-executed phase); later phases
  are intentionally outlined and expanded on entry.
