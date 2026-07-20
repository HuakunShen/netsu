# netsu Rewrite — Design

Date: 2026-07-20
Status: approved pending final review

## 1. Background & Goals

netsu is an iperf3-like network speed test tool (client/server, measure real
link throughput). The current TypeScript implementation is stuck mid-refactor
(does not compile, TCP download transfers 0 bytes, tests red), the Go version
only has TCP, and the Rust directory is an empty shell.

This rewrite replaces everything:

- **Two first-class native implementations**: TypeScript (Node APIs, published
  to npm/JSR) and Rust (tokio, single binary).
- **The wire protocol is iperf3's wire protocol.** netsu interoperates with
  official iperf3 in both directions (netsu client ↔ iperf3 server, iperf3
  client ↔ netsu server). TS ↔ Rust interop follows for free.
- **WebSocket transport as netsu's extension** (iperf3 cannot do this): the
  identical protocol state machine tunneled over WS binary frames. Enables
  HTTP-proxy traversal and a future browser client.
- The Go implementation is deleted (recoverable from git history).

### Non-goals (phase 2 or never)

- `--bidir`, `-n`/`-k` (byte/block count modes), TCP window/MSS tuning,
  `--omit` warmup, SCTP, iperf3 authentication (RSA), `-4`/`-6` flags.
- Prebuilt Rust binaries on GitHub Releases (later; `cargo install` first).
- Browser client (the WS design keeps the door open; not built now).

## 2. Feature Scope ("standard tier")

- Transports: **tcp**, **udp**, **ws** × directions: normal (client sends) and
  **reverse `-R`** (server sends).
- **Per-interval reports** (default 1s), iperf3-style one line per second.
- **UDP statistics**: packet loss, out-of-order count, RFC 1889 jitter — via
  per-packet timestamp + sequence number payload, same as iperf3.
- **UDP pacing**: `-b` bandwidth limit, default 1 Mbps (matches iperf3).
  Unpaced UDP is a footgun and an attack vector.
- **Server-side results exchange**: both sides swap their view of the test
  (EXCHANGE_RESULTS), so reported speed reflects what the receiver actually
  got — the key to honest measurement.
- **Parallel streams `-P`** (default 1, max 128).
- **`--json`** output, field names aligned with iperf3's JSON schema so
  existing tooling can parse it.
- TCP retransmit counts from `TCP_INFO` on Linux; gracefully null elsewhere
  (iperf3 does the same).

## 3. Repository Layout & Distribution

```
netsu/
├── PROTOCOL.md            # the wire protocol, documented from iperf3 source;
│                          # + the netsu WS tunneling extension. Single source
│                          # of truth for both implementations.
├── packages/netsu/        # NEW TS package, scaffolded with create-tsdown
│                          # (default template). Old package dir deleted.
├── netsu-rs/              # Rust crate: lib + bin. Rewritten from scratch.
├── interop/               # docker-compose e2e + interop test matrix
└── go/                    # DELETED
```

- **TS package**: keeps npm name `netsu` and JSR name `@hk/netsu`, version
  **0.2.0** (breaking: new wire protocol). tsdown default template: ESM-only,
  `exports: true` (auto-generated exports map), dts via tsgo, vitest, and a
  `typecheck` script that runs in CI. Build with bun, not pnpm.
- **Rust crate**: name `netsu` if free on crates.io, else `netsu-rs` (verify
  at implementation time). Installable via `cargo install`.
- Old `packages/netsu` contents, `go/`, and stray `netsu-rs/target` artifacts
  are removed. `netsu-rs` gets a proper `.gitignore`.

## 4. Protocol

**Principle: the control protocol is iperf3's, byte for byte. WS only swaps
the transport underneath.** Details are documented in `PROTOCOL.md` during
implementation, verified against the cloned iperf3 source (state constants in
`iperf_api.h`, framing in `iperf_api.c`).

Test lifecycle skeleton:

```
client                                 server
  │ ── TCP connect (control) ────────► │
  │ ── cookie (37-byte ASCII UUID) ──► │   cookie = session identity
  │ ◄── state: PARAM_EXCHANGE (9) ──── │
  │ ── [4B BE len][params JSON] ─────► │
  │ ◄── state: CREATE_STREAMS (10) ─── │
  │ ══ open N data conns, each sends the same cookie ══► │
  │ ◄── state: TEST_START (1) ──────── │
  │ ◄── state: TEST_RUNNING (2) ────── │
  │ ══════ data plane at full rate ═══ │   (direction flips under -R)
  │ ── state: TEST_END (4) ──────────► │
  │ ◄── state: EXCHANGE_RESULTS (13) ─ │
  │ ◄─ [4B len][results JSON] both ways ─► │
  │ ◄── state: DISPLAY_RESULTS (14) ── │
  │ ── state: IPERF_DONE (16) ───────► │
```

- Control messages: single signed state bytes; JSON payloads framed with a
  4-byte big-endian length prefix (`JSON_write`/`JSON_read` in iperf3).
- Error states: `ACCESS_DENIED (-1)`, `SERVER_ERROR (-2)`.
- **Control channel is always TCP** (iperf3 mode) or entirely inside one WS
  connection (netsu mode). UDP is data-plane only.
- **UDP stream setup** (verified against `iperf_udp.c`): during
  CREATE_STREAMS the client sends a 4-byte `UDP_CONNECT_MSG` (wire bytes
  `39 38 37 36` = `"9876"`, i.e. `0x39383736` read big-endian); the server
  `recvfrom`s it, `connect()`s the socket to that peer (kernel pins the remote
  address), and replies `UDP_CONNECT_REPLY` (wire bytes `36 37 38 39` =
  `"6789"`). These are raw wire bytes, not integers to byte-swap — see
  `PROTOCOL.md`'s "UDP specifics", which is the authority both implementations
  are verified against. (An earlier draft here had these two values swapped.)
  Hellos are only accepted during CREATE_STREAMS of an active test, and the
  connected socket rejects stray sources — this kills the reflection-attack
  surface of the old design without any cookie in the UDP path.
- **UDP data packets** carry sec/usec timestamp + packet sequence number →
  loss, reordering, RFC 1889 jitter.
- **WS mode (netsu extension)**: WS binary frames are treated as a plain byte
  pipe; the same codec and state machine run unchanged. Control and each data
  stream get their own WS connection. Official iperf3 simply can't connect to
  a WS port — expected.
- **Server semantics match iperf3**: one test at a time; a second client
  during an active test receives `ACCESS_DENIED`. This also eliminates the
  shared-mutable-state bugs of the old implementation by construction.

## 5. Implementation Architecture (mirrored TS/Rust)

Both implementations use the same module layout; understanding one means
understanding the other.

```
src/
├── protocol/         # pure logic, never touches a socket — unit-test target
│   ├── states        # state constants
│   ├── cookie        # 37-byte cookie generate/verify
│   ├── framing       # [4B BE len][JSON] codec + state-byte read/write
│   ├── params        # PARAM_EXCHANGE types  (TS: valibot / Rust: serde)
│   └── results       # EXCHANGE_RESULTS types, iperf3 field names
├── transport/        # "byte pipe" abstraction (TS interface / Rust trait)
│   ├── tcp
│   ├── udp           # data plane only; token-bucket pacing lives here
│   └── ws            # WS binary frames ↔ byte pipe  (ws / tokio-tungstenite)
├── client            # control state machine, client role
├── server            # control state machine, server role (single-test lock)
├── streams/          # data senders/receivers: TCP backpressure-driven,
│                     # UDP timestamp+seq, byte/packet accounting
├── stats             # interval aggregation, jitter, loss, Mbps
├── index / lib       # library API
└── cli / main        # TS: citty / Rust: clap
```

Hard boundary: `protocol/` and `stats` are I/O-free (everything flows through
the byte-pipe abstraction), so the state machine and the math are unit-testable
without sockets. Every bug class in the old code (backpressure, TCP message
coalescing, shared state, leaked timers) died on this boundary being absent.

Rust stack: `tokio`, `tokio-tungstenite`, `serde`/`serde_json`, `clap`,
`thiserror` (lib errors) + `anyhow` (CLI).

## 6. CLI & Library API

Subcommand style kept; flag semantics mirror iperf3:

```bash
netsu server [-p 5201] [--ws]
netsu client <host> [-p 5201] [-u | --ws]
             [-t 10] [-P 1] [-R] [-b 1M] [-i 1] [--json]
```

- `-R` replaces the old `--type download`: default is client-sends (upload),
  `-R` means server-sends — identical to iperf3.
- Human output mimics iperf3: one line per interval, sender/receiver summary
  lines at the end. `--json` emits the iperf3-aligned structure.

Library API (TS shown; Rust is symmetric):

```ts
const server = await startServer({ port, transport: "tcp" | "ws" });
// server.close()

const result = await runClient("host", {
  transport, udp, reverse, duration, parallel, bandwidth,
  onInterval: (report) => {},   // per-second callback
});
// result: full test result incl. both sides' views, per-stream intervals
```

## 7. Error Handling

- Any control-channel failure (cookie timeout, JSON parse error, illegal
  state byte) → send `SERVER_ERROR`, close, server returns to idle.
- Client maps failures to phase-tagged errors ("server rejected during param
  exchange"), not bare ECONNRESET.
- Single `finish()` path per test tears down all timers/sockets exactly once
  — fixes the old leaked-timer / double-resolve class.
- Bounds on client-controlled input: `blksize` ≤ 1 MiB, `parallel` ≤ 128,
  JSON length prefix ≤ 64 KiB (iperf3's JSON_read has the same max_size idea).
- Server busy → `ACCESS_DENIED`, connection closed, active test undisturbed.

## 8. Testing

1. **Unit** (per implementation): framing round-trip, cookie, jitter/loss
   math, params/results serialization.
2. **Same-implementation integration**: TS↔TS and Rust↔Rust over localhost,
   matrix {tcp, udp, ws} × {normal, -R} × {P=1, P=3}.
3. **Docker e2e / interop matrix** (`interop/`, docker compose, one network):
   - Containers: `netsu-ts` (oven/bun image running source),
     `netsu-rs` (debian-slim/alpine + prebuilt Linux binary),
     `iperf3` (alpine, `apk add iperf3` — the protocol referee).
   - **Rust cross-compilation**: on macOS build
     `aarch64-unknown-linux-musl` (same arch as Apple-Silicon containers, no
     emulation; musl static = no glibc issues). In CI (Linux x86_64) build
     `x86_64-unknown-linux-musl`. A script picks the target from host arch.
   - Matrix: every client container × every server container × {tcp, udp, ws}
     (official iperf3 participates in tcp/udp only). Assert both sides agree
     on bytes transferred and speeds are sane (> 0, < absurd).
   - `bun run e2e` locally; CI runs the same script.
4. **CI gates**: `tsc --noEmit`, `cargo clippy`, all unit + integration tests
   must pass before publish; publish only from `main` (fixes the current
   publish-broken-code-from-develop hole).

## 9. Decisions Log

| Decision | Choice |
|---|---|
| Languages | TS + Rust; Go deleted |
| Architecture | Two native implementations + shared PROTOCOL.md (option A) |
| Protocol | iperf3 wire protocol as the one protocol; WS tunnels it |
| Interop | Full: TS ↔ Rust ↔ official iperf3, CI-enforced |
| Feature tier | Standard (intervals, UDP stats, -P, -R, --json, -b) |
| TS tooling | fresh create-tsdown scaffold, bun, ESM-only, npm name `netsu` @ 0.2.0 |
| Rust tooling | tokio single crate, clap CLI, musl cross-compile for e2e |
| E2E | docker compose, bun image for TS, cross-compiled musl binary for Rust |
