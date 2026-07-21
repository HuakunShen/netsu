# WebRTC Direct-Only Transport Implementation Plan

> **Execution note:** This plan is self-contained in netsu. Complete the
> in-repository Cloudflare signaling plan first. Run its Worker with
> Wrangler/workerd; do not create a Bun WebSocket server.

**Goal:** Add an opt-in reliable ordered WebRTC DataChannel transport to
`netsu-rs` that measures only direct ICE paths, uses the public/local
Cloudflare RendezKey Worker for signaling, interoperates with Chromium, and has
bounded local, container, and public-network tests.

**Architecture:** One `RTCPeerConnection` contains one control DataChannel and
`parallel` payload DataChannels. The netsu client creates channels and the
offer; the server answers. The in-repository `apps/rendez-key` Durable Object exchanges
only SDP and ICE messages. STUN is optional. TURN is unsupported: a relay or
unknown selected pair fails before payload transfer, and ICE failure produces a
warning rather than a misleading throughput result.

**Runtime boundary:** Cloudflare Worker APIs, D1, Durable Objects, and WebSocket
Hibernation are tested under Wrangler/workerd and
`@cloudflare/vitest-pool-workers`. Bun may run workspace scripts; Bun is not the
signaling server runtime.

**Pinned stack:** Rust 2024, Tokio, `webrtc =0.17.1`, `tokio-tungstenite`,
`serde`, `serde_json`, `secrecy`; RendezKey's current Hono/Wrangler/Vitest stack;
Playwright `1.61.1` and `mcr.microsoft.com/playwright:v1.61.1-noble`; Docker
Compose; Linux `tc/netem`/network policy for failure injection.

**Spec:** `docs/specs/2026-07-22-quic-webrtc-transports.md`

**Prerequisite plan:**
`docs/plans/2026-07-22-cloudflare-webrtc-signaling.md`

## 0. Execution contract

- Preserve existing TCP, UDP, WebSocket, iroh, TypeScript, and iperf3 behavior.
- Do not modify `netsu-rs/src/tui.rs`; it contains unrelated user work.
- Do not add WebRTC to `packages/netsu` in v1.
- Keep WebRTC behind a non-default Cargo feature.
- Never contact Google STUN or a public signaling endpoint in unit/PR CI.
- Never configure a TURN URL, username, credential, or relay policy.
- All setup waits must have explicit deadlines and cleanup assertions.
- Container throughput is correctness evidence, not performance evidence.
- Each task begins with a failing test and ends with the listed verification.
- Run `git status --short` before every task; do not stage
  unrelated pre-existing changes.

## 1. Planned file map

Create in netsu:

- `netsu-rs/src/transport/webrtc/mod.rs`
- `netsu-rs/src/transport/webrtc/config.rs`
- `netsu-rs/src/transport/webrtc/signaling.rs`
- `netsu-rs/src/transport/webrtc/peer.rs`
- `netsu-rs/src/transport/webrtc/pipe.rs`
- `netsu-rs/src/transport/webrtc/channel.rs`
- `netsu-rs/src/transport/webrtc/metrics.rs`
- `netsu-rs/tests/webrtc_pipe.rs`
- `netsu-rs/tests/webrtc_signaling.rs`
- `netsu-rs/tests/webrtc_transport.rs`
- `netsu-rs/tests/webrtc_cli.rs`
- `interop/transports/docker-compose.yml`
- `interop/transports/Dockerfile.rs`
- `interop/transports/e2e.sh`
- `interop/transports/run-matrix.ts`
- `interop/transports/netem-entrypoint.sh`
- `interop/transports/browser/package.json`
- `interop/transports/browser/src/client.ts`
- `interop/transports/browser/tests/webrtc.spec.ts`
- `interop/transports/browser/Dockerfile`
- `interop/transports/README.md`
- `scripts/dev-webrtc-signal.sh`

Modify in netsu:

- `netsu-rs/Cargo.toml`
- `netsu-rs/Cargo.lock`
- `netsu-rs/src/transport/mod.rs`
- `netsu-rs/src/client.rs`
- `netsu-rs/src/server.rs`
- `netsu-rs/src/error.rs`
- `netsu-rs/src/protocol/results.rs`
- `netsu-rs/src/main.rs`
- `netsu-rs/scripts/verify.sh`
- `PROTOCOL.md`
- `README.md`
- `netsu-rs/README.md`
- `interop/README.md`
- `package.json`
- `.github/workflows/ci.yml`
- `.github/workflows/e2e.yml`

RendezKey signaling files are enumerated in the prerequisite plan.

## Task 1: Add compile-gated configuration and protocol types

**Files:** `Cargo.toml`, `transport/mod.rs`, `transport/webrtc/{mod,config,
signaling}.rs`, `error.rs`, `tests/webrtc_cli.rs`.

1. Add a failing compile/test fixture that expects `Transport::WebRtc`,
   `WebRtcOptions`, `SetupPhase` values, and typed signaling messages.
2. Add feature `webrtc` with exact dependency pins. Verify the default feature
   graph does not include `webrtc` or `tokio-tungstenite`.
3. Define:

   ```rust
   pub struct WebRtcOptions {
       pub signal_url: url::Url,
       pub stun_urls: Vec<String>,
       pub include_addresses: bool,
   }
   ```

   Treat the signal URL as an HTTP service base. Reject non-`http`/`https`
   signal URLs, non-`stun` ICE URLs, empty URLs, more than four STUN URLs, and
   any `turn:`/`turns:` input. Derive `ws`/`wss` only for the room socket.

4. Mirror signaling v1 with externally tagged serde types: room creation,
   bind, description, candidate, end-of-candidates, leave, bound, peer-ready,
   peer-left, and structured errors. Apply string/frame/candidate limits before
   allocation where possible.
5. Add `NetsuError::Setup { transport, phase, detail }`. Ensure `detail` is
   sanitized and does not include SDP, candidate strings, addresses, or the
   listener secret.

**Verify:**

```bash
cd netsu-rs
cargo fmt --check
cargo test --no-default-features
cargo test --features webrtc --test webrtc_cli
cargo tree --no-default-features | rg 'webrtc|tokio-tungstenite' && exit 1 || true
```

Expected: default tree scan is empty; feature test passes.

**Commit boundary:** `feat(webrtc): define direct-only configuration and wire types`

## Task 2: Implement a bounded signaling client against the real Worker

**Files:** `transport/webrtc/signaling.rs`, `tests/webrtc_signaling.rs`.

1. Add failing integration tests that start the in-repository RendezKey with
   the netsu wrapper, which runs `wrangler dev --port 18787 --var
PUBLIC_SIGNAL_CREATE:true` in `apps/rendez-key`, and prove:
   - create room returns code, secret, expiry;
   - listener binds with its secret;
   - joiner binds with the code;
   - offer/answer/candidates traverse in order;
   - listener secret never appears in process output;
   - a second joiner, wrong secret, expired room, oversized frame, and binary
     frame fail with exact protocol codes;
   - child Wrangler process is killed on test success, panic, and timeout.
2. Create `scripts/dev-webrtc-signal.sh`. It validates the local
   `apps/rendez-key/wrangler.jsonc`, starts the package through the root Bun
   script, uses an explicit temporary `--persist-to` directory, polls
   `/healthz`, and traps EXIT/INT/TERM to stop Wrangler and remove only that
   temporary state. Missing source is a hard failure locally and in CI.
3. Implement `SignalingClient::create_listener` and `join`. Enforce:
   connect 10s, bind 10s, offer/answer 15s, frame 64 KiB, candidate 16/peer, and
   total room bytes 1 MiB.
4. Store the listener secret in a secret wrapper, zero/drop it after bind, and
   redact WebSocket errors before mapping them to public errors.
5. Close with `leave`, then WebSocket close; abort receive tasks after a
   two-second grace period.

**Verify:**

```bash
cd /Users/hk/Dev/netsu
bun run signal:test
bun run signal:typecheck

cd /Users/hk/Dev/netsu/netsu-rs
cargo test --features webrtc --test webrtc_signaling -- --nocapture
pgrep -f 'wrangler dev.*18787' && exit 1 || true
```

**Commit boundary:** `feat(webrtc): connect to RendezKey signaling rooms`

## Task 3: Implement DataChannel byte adapters with backpressure

**Files:** `transport/webrtc/{pipe,channel}.rs`, `tests/webrtc_pipe.rs`.

1. Write failing adapter tests with a deterministic fake DataChannel:
   `read_exact` across message boundaries, multiple reads from one message,
   close after buffered bytes, text rejection, 1 MiB receive cap, 16 KiB send
   fragmentation, and write after close.
2. Implement `WebRtcPipe` for control and `WebRtcChannel` for payload using the
   existing `BytePipe` and `DataChannel` traits.
3. Never send one SCTP message over 16 KiB, even if the browser advertises a
   larger maximum. Keep `-l` as the application chunk size.
4. Add event-driven backpressure: pause at 4 MiB buffered, resume at 1 MiB, and
   never poll in a zero-delay loop. Tests use a paused Tokio clock.
5. Drain to zero for at most five seconds before close. A drain timeout is an
   error because byte reconciliation becomes ambiguous.

**Verify:**

```bash
cd netsu-rs
cargo test --features webrtc --test webrtc_pipe
cargo clippy --features webrtc --all-targets -- -D warnings
```

**Commit boundary:** `feat(webrtc): adapt ordered DataChannels to netsu streams`

## Task 4: Build the direct-only PeerConnection state machine

**Files:** `transport/webrtc/{peer,metrics,mod}.rs`, new focused unit tests.

1. Add tests for one control channel, exactly `P` data labels, duplicate/unknown
   labels, wrong subprotocol, candidate callbacks, and deterministic shutdown.
2. Configure reliable ordered in-band-negotiated DataChannels with protocol
   `netsu/iperf3-webrtc/1`.
3. Client creates control before its offer and payload channels only at
   `CREATE_STREAMS`; server accepts remote-created channels and validates labels.
4. Trickle ICE through signaling. Apply remote descriptions before candidates
   that require them; buffer only within the documented count/byte caps.
5. Enforce setup deadlines: gather 15s, connected 20s, channels 10s, close 2s.
6. Read the selected candidate pair before `TEST_START`. Accept only host,
   srflx, or prflx with a known direct pair. If either side is relay or the pair
   is unknown, send no payload, close, and return `direct_path_required`.
7. Capture setup durations, candidate types, ICE protocol, RTT if exposed,
   messages/bytes if exposed, and backpressure duration. Addresses are redacted
   unless explicitly requested.

**Verify:**

```bash
cd netsu-rs
cargo test --features webrtc transport::webrtc
cargo clippy --features webrtc --all-targets -- -D warnings
```

**Commit boundary:** `feat(webrtc): establish bounded direct peer connections`

## Task 5: Bind WebRTC to the existing server/client protocol

**Files:** `client.rs`, `server.rs`, `protocol/results.rs`,
`tests/webrtc_transport.rs`.

1. Add the four 1-second matrix tests: upload/reverse × parallel 1/4.
2. Add lifecycle tests: two sequential sessions, concurrent server-busy,
   abrupt peer loss, signaling loss after connection, missing payload channel,
   and setup cancellation.
3. Add a test that injects a relay/unknown selected-pair result and asserts no
   `run_sender`/`run_receiver` invocation.
4. Reuse the authoritative cookie/state/PARAM_EXCHANGE/CREATE_STREAMS/
   EXCHANGE_RESULTS sequence. Do not fork the protocol state machine.
5. Normalize connection diagnostics via `ConnectionInfo::WebRtc` while keeping
   existing iroh JSON snapshots backward-compatible.
6. Reconcile application bytes within 2% after graceful drain and assert finite
   positive throughput only for successful direct sessions.

**Verify:**

```bash
cd netsu-rs
for i in 1 2 3; do
  cargo test --features webrtc --test webrtc_transport -- --nocapture || exit 1
done
```

**Commit boundary:** `feat(webrtc): run netsu tests over direct DataChannels`

## Task 6: Add CLI UX and direct-failure warnings

**Files:** `main.rs`, `tests/webrtc_cli.rs`, `README.md`, `netsu-rs/README.md`.

1. Add failing CLI snapshots for:
   - server `--webrtc --signal-url <ws-url> [--stun <url>]`;
   - client `<code> --webrtc --signal-url <ws-url> [--stun <url>]`;
   - mutual exclusion with `--udp`, `--ws`, `--iroh`, and `--quic`;
   - feature-not-compiled error;
   - rejection of TURN URLs and relay options.
2. Server creates a signaling room and prints the formatted code only after its
   listener WebSocket is bound. Human output may show expiry, never the secret.
3. On ICE direct failure print one actionable warning to stderr:

   ```text
   warning: WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run
   ```

   JSON mode emits a structured setup error object and no `bits_per_second`.

4. Exit codes: `0` success, `2` CLI/config, `3` signaling/setup timeout,
   `4` direct-path unavailable, existing runtime codes otherwise unchanged.
5. Document optional Google STUN only in a manual public example and note its
   external availability/privacy boundary.

**Verify:**

```bash
cd netsu-rs
cargo test --features webrtc --test webrtc_cli
cargo run --features webrtc -- server --help | rg 'webrtc|signal-url|stun'
cargo run --features webrtc -- client --help | rg 'webrtc|signal-url|stun'
cargo run --features webrtc -- client --help | rg 'turn|relay' && exit 1 || true
```

**Commit boundary:** `feat(cli): expose direct-only WebRTC testing`

## Task 7: Add an independent Chromium peer

**Files:** `interop/transports/browser/**`.

1. Pin Playwright and the browser image by exact version. Do not use `latest`.
2. Implement the signaling protocol independently with browser `WebSocket` and
   `RTCPeerConnection`; do not import Rust-generated protocol code.
3. Implement real netsu cookie/state/JSON control framing, `bufferedAmount`/
   `bufferedamountlow`, binary ArrayBuffers, channel label validation, and
   final JSON export.
4. Test Chromium→Rust upload/reverse and parallel 1/4.
5. Add failure tests for wrong listener secret, malformed offer, missing
   channel, and blocked direct connectivity. The blocked case must display the
   same semantic warning and produce no throughput.

**Verify:**

```bash
docker build -f interop/transports/browser/Dockerfile \
  -t netsu-webrtc-browser:test interop/transports/browser
docker run --rm netsu-webrtc-browser:test npx playwright test
```

**Commit boundary:** `test(webrtc): add Chromium protocol peer`

## Task 8: Build the self-contained container E2E harness

**Files:** `interop/transports/**`, root `package.json`.

The harness builds `apps/rendez-key` from the same netsu checkout as an
additional Compose context. It starts the real Worker with Wrangler/workerd;
there is no Bun signaling server and no public network dependency.

Services:

- `signal`: pinned Node image, in-repository app, `wrangler dev --ip 0.0.0.0
--port 8787 --persist-to <container-temp-state> --var
PUBLIC_SIGNAL_CREATE:true` executed in `apps/rendez-key`;
- `rs-server` and `rs-client`: netsu built with `webrtc`;
- `browser`: pinned Playwright image;
- an isolated network/profile for direct-connect failure injection.

Required cells:

| Peer           | Direction | Parallel | Network            | Expected                      |
| -------------- | --------- | -------: | ------------------ | ----------------------------- |
| Rust↔Rust     | upload    |      1,4 | bridge direct      | success                       |
| Rust↔Rust     | reverse   |      1,4 | bridge direct      | success                       |
| Chromium↔Rust | upload    |      1,4 | bridge direct      | success                       |
| Chromium↔Rust | reverse   |        1 | bridge direct      | success                       |
| Rust↔Rust     | upload    |        1 | UDP/direct blocked | exit 4, no throughput         |
| Chromium↔Rust | upload    |        1 | UDP/direct blocked | direct failure, no throughput |

Assertions:

- Worker `/healthz` and room-create routes are ready before peers launch;
- success cells report `transport=webrtc`, `path=direct`, exact stream count,
  non-zero bytes, finite throughput, and byte drift ≤2% Rust/≤5% browser;
- no success cell selects relay or unknown;
- blocked cells finish inside the ICE deadline plus five seconds, include the
  warning/error code, and have no throughput fields;
- no test contacts a non-Compose IP except package downloads during image build;
- teardown leaves no Compose resources or Wrangler child processes;
- failure artifacts include redacted peer/Worker logs, candidate types, JSON,
  and Compose state under `interop/transports/results/`.

**Verify:**

```bash
bun run e2e:webrtc
docker compose -f interop/transports/docker-compose.yml ps -aq | rg . && exit 1 || true
rg -n 'listener_secret|"sdp"|candidate:' interop/transports/results && exit 1 || true
```

Run the complete matrix three times before enabling CI.

**Commit boundary:** `test(e2e): cover direct and blocked WebRTC paths`

## Task 9: Add self-contained CI

**Files:** `.github/workflows/ci.yml`, `.github/workflows/e2e.yml`,
`scripts/verify.sh`, docs.

1. Add feature-specific fmt/clippy/test to regular CI; keep default and publish
   dry-run gates.
2. Add `extended-webrtc` job with a 35-minute timeout. It uses only the current
   netsu checkout and builds the Worker and peers from the same revision.
3. Run RendezKey typecheck/test/deploy-dry before netsu E2E. If its
   conformance suite fails, do not run transport cells.
4. Cache Rust/Node dependencies, not Wrangler local state or DO storage.
5. Upload redacted artifacts only on failure and run Compose teardown in an
   `always()` step.
6. Keep public smoke out of PR CI. A manual workflow may accept a signaling URL
   and two self-hosted runners on distinct networks, but secrets must remain in
   repository/environment secrets.

**Verify locally with the exact CI commands**, then inspect workflow YAML with
`actionlint` if available.

**Commit boundary:** `ci(webrtc): gate Worker-backed direct transport E2E`

## Task 10: Public-network smoke without TURN

This is a release procedure, not an automated PR test.

1. Deploy the in-repository Worker using its prerequisite plan and verify
   `/healthz`, room creation, WebSocket bind, and rate limiting.
2. On device A:

   ```bash
   netsu server --webrtc \
     --signal-url https://rendez-key.xc.huakun.tech/v1/signal \
     --stun stun:stun.l.google.com:19302
   ```

3. On a genuinely different network, device B joins the printed code with the
   same signaling and STUN URLs.
4. Record candidate types, protocol, setup phases, OS/network type, application
   bytes, and throughput. Redact addresses and all SDP/candidates.
5. Repeat on a restrictive network. Failure is an expected supported outcome:
   verify exit code 4, warning text, bounded cleanup, and no throughput.
6. Do not “fix” a restrictive-network failure by adding TURN. A future relay
   benchmark would be a separate transport/path mode with separate semantics.

## Task 11: Final documentation and full verification

Update protocol and user docs with:

- signaling vs STUN vs TURN responsibilities;
- Worker deployment/runtime boundary;
- direct-only policy and failure meaning;
- commands for local Wrangler, Docker E2E, LAN, and public smoke;
- privacy/redaction rules;
- controlled-container evidence versus real-network evidence.

Run from clean checkouts (apart from known unrelated user changes):

```bash
cd /Users/hk/Dev/netsu
bun run signal:typecheck
bun run signal:test
bun run signal:deploy:dry

cd /Users/hk/Dev/netsu/netsu-rs
cargo fmt --check
cargo clippy --no-default-features --all-targets -- -D warnings
cargo clippy --features webrtc --all-targets -- -D warnings
cargo test --no-default-features
cargo test --features webrtc
cargo publish --dry-run

cd /Users/hk/Dev/netsu
bun run test
bun run e2e
bun run e2e:webrtc
git diff --check
```

Inspect staged names before each commit. Do not stage the pre-existing
`netsu-rs/src/tui.rs` change.

## Acceptance checklist

- [ ] Default netsu build does not include WebRTC dependencies.
- [ ] Rust↔Rust matrix passes upload/reverse and parallel 1/4.
- [ ] Chromium speaks the real protocol and passes required direct cells.
- [ ] Selected candidate pair is proven direct before payload.
- [ ] Relay and unknown paths are rejected before sender/receiver start.
- [ ] Blocked direct path warns, exits boundedly, and emits no throughput.
- [ ] Signaling tests exercise the actual Worker/DO implementation.
- [ ] Container tests use Wrangler/workerd, not Bun WebSocket APIs.
- [ ] No automated test contacts public STUN/signaling services.
- [ ] Worker and transport E2E use the same netsu revision.
- [ ] Secrets, SDP, candidates, and addresses pass redaction scans.
- [ ] Existing TCP/UDP/WS/iroh/core E2E remains green.
- [ ] Public direct success and restrictive-network failure are recorded before release.

## Recommended execution order

1. Complete and deploy-test the Cloudflare signaling plan.
2. Tasks 1–4: compile boundary, signaling client, adapters, peer state machine.
3. Tasks 5–6: protocol integration and CLI.
4. Tasks 7–8: Chromium and container E2E.
5. Tasks 9–11: CI, public smoke, docs, full verification.

Do not parallelize tasks that edit `client.rs`, `server.rs`, `main.rs`, or the
same workflow. Chromium fixture work may run in parallel only after signaling
v1 is frozen and its golden fixtures are committed.
