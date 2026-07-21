# Native QUIC and WebRTC Transport Specification

**Status:** Approved for planning
**Date:** 2026-07-22
**Scope owner:** netsu Rust implementation (`netsu-rs`)
**Protocol status:** netsu extensions; not iperf3-compatible transports

## 1. Summary

netsu will add two opt-in transports:

1. **Native QUIC**: a fixed-address QUIC baseline using Quinn, with one QUIC
   connection per test, one control bidirectional stream, and one bidirectional
   stream per requested data stream.
2. **WebRTC DataChannel**: a NAT-traversing/browser-interoperable baseline using
   one `RTCPeerConnection`, one reliable ordered control DataChannel, and one
   reliable ordered DataChannel per requested data stream.

Both transports reuse netsu's existing iperf3-derived control state machine and
result exchange. They are netsu-only bindings: official iperf3 cannot dial them.
The existing TCP, UDP, WebSocket, and iroh behavior must remain unchanged.

The first implementation is deliberately Rust-first. Native QUIC and native
WebRTC server/client support land in `netsu-rs`. A small browser fixture is
required for WebRTC interoperability E2E, but the Node-oriented
`packages/netsu` public API is not expanded in this project. TUI integration is
also out of scope because `netsu-rs/src/tui.rs` has concurrent user work.

## 2. Goals

- Measure raw QUIC separately from iroh's identity, discovery, NAT traversal,
  and relay layers.
- Measure WebRTC DataChannel throughput only over direct ICE paths. STUN may
  discover server-reflexive candidates, but TURN relay is intentionally absent.
- Preserve the existing duration, reverse, parallel-stream, interval, and JSON
  semantics wherever transport semantics allow it.
- Keep the default Rust binary dependency-light: neither transport compiles
  unless its Cargo feature is enabled.
- Make all correctness gates automatable without external network services.
- Test both transports in local-process integration tests and Linux containers.
- Validate WebRTC against a real Chromium implementation, not only webrtc-rs
  against itself.
- Separate protocol correctness assertions from performance observations.

## 3. Non-goals

- No iperf3 interoperability over QUIC or WebRTC.
- No HTTP/3, WebTransport, MASQUE, or QUIC DATAGRAM support.
- No QUIC 0-RTT, session resumption, connection migration benchmark, or custom
  congestion-controller selection in the first release.
- No WebRTC audio/video tracks.
- No unordered or partially reliable WebRTC mode in the first release.
- No TURN support or relay fallback in v1.
- No standalone Bun signaling runtime.
- No production-grade user identity, account system, or billing in signaling.
- No TypeScript/Node QUIC backend.
- No changes to the TUI in these plans.
- No absolute throughput threshold in CI or container E2E.

## 4. Design alternatives and decision

### Alternative A: force every transport through independent socket-like connections

This would create one QUIC connection or one PeerConnection per `-P` stream.
It looks similar to the existing TCP/WS topology, but it measures repeated
handshakes and independent congestion controllers instead of the multiplexing
behavior for which QUIC and WebRTC are normally chosen.

**Rejected as the default.** It may become an explicit research mode later.

### Alternative B: create entirely separate speed protocols and result schemas

This gives each transport complete freedom but duplicates netsu's proven
PARAM_EXCHANGE, lifecycle, stream accounting, reverse-mode, interval reporting,
and result reconciliation.

**Rejected for reliable transports.** A separate protocol will be appropriate
if unordered/partial-reliability WebRTC is added later.

### Alternative C: bind the existing state machine to multiplexed transports

One logical connection contains a control channel and `N` data channels. The
existing `BytePipe`, `DataChannel`, `run_sender`, `run_receiver`, parameter
exchange, and result exchange remain authoritative.

**Selected.** This matches the existing iroh binding, minimizes duplicated
protocol logic, and makes the QUIC/iroh/WebRTC comparison interpretable.

## 5. Existing contracts that remain authoritative

- `PROTOCOL.md` remains the wire reference for cookie bytes, state values, JSON
  framing, PARAM_EXCHANGE, EXCHANGE_RESULTS, and lifecycle.
- `netsu-rs/src/protocol/pipe.rs::BytePipe` remains the ordered control-channel
  abstraction.
- `netsu-rs/src/streams/channel.rs::DataChannel` remains the reliable bulk
  payload abstraction.
- `netsu-rs/src/streams/runner.rs` remains responsible for application payload
  counters and sender/receiver loops.
- UDP remains separate and packet-based. `--udp` is mutually exclusive with
  `--quic`, `--iroh`, `--ws`, and `--webrtc`.
- The client still opens all data channels. The server accepts them only during
  `CREATE_STREAMS` and rejects unexpected channels.
- One server process serves one active test at a time. Every error path must
  release that lock.

## 6. Shared transport model

### 6.1 Transport enum and features

`netsu-rs` adds:

```rust
pub enum Transport {
    Tcp,
    #[cfg(feature = "ws")]
    Ws,
    #[cfg(feature = "iroh")]
    Iroh,
    #[cfg(feature = "quic")]
    Quic,
    #[cfg(feature = "webrtc")]
    WebRtc,
}
```

Cargo features:

```toml
quic = ["dep:quinn", "dep:rustls", "dep:rcgen", "dep:sha2"]
webrtc = ["dep:webrtc", "dep:tokio-tungstenite", "dep:futures-util", "dep:base64", "dep:secrecy"]
```

The implementation plan must pin Quinn to the version already resolved in the
lock file when implementation begins. The reviewed checkout resolves Quinn
`0.11.11`. WebRTC uses the Tokio-coupled webrtc-rs maintenance line pinned to
`webrtc = "=0.17.1"`; this is intentionally chosen over the newer sans-I/O
rewrite for the first implementation because netsu is already Tokio-based and
the maintenance line has the simpler, stable async integration surface. The
browser E2E gate protects against library-specific behavior.

PEM loading uses the `rustls::pki_types::pem::PemObject` API re-exported by
rustls. Do not add `rustls-pemfile`: its upstream repository is archived and
RustSec classifies every release as unmaintained (`RUSTSEC-2025-0134`).

### 6.2 CLI selection

The following flags are mutually exclusive:

```text
--ws | --iroh | --quic | --webrtc
```

`--udp` is valid only when none of those flags is present. Selecting a feature
that was not compiled must return the existing feature-specific error style
and a non-zero exit code.

### 6.3 Common test semantics

- Upload is the default.
- `-R/--reverse` makes the listener send application payload.
- `-P/--parallel` creates exactly that many QUIC streams or DataChannels.
- One transport connection is used for the entire test.
- `-t/--time`, `-l/--len`, `-i/--interval`, JSON output, and server reporting
  keep their existing meanings.
- Application payload bytes, not UDP/QUIC/WebRTC framing bytes, are counted.
- Handshake/setup time is never included in the throughput duration.
- A test starts only after the control channel and all requested data channels
  are ready on both peers.
- A sender stops creating payload at the duration deadline, drains accepted
  transport buffers within a bounded grace period, then exchanges results.

## 7. Native QUIC binding

### 7.1 Stack and addressing

- Implementation: Quinn over UDP with TLS 1.3.
- Addressing: `host` plus the existing `-p/--port`; default port remains 5201.
- ALPN: `netsu/iperf3-quic/1`.
- One QUIC `Connection` per test.
- The dialer/client opens all bidirectional streams.

### 7.2 Stream mapping

```text
QUIC connection
├── first client-opened bi stream: control BytePipe
├── next P client-opened bi streams: data DataChannel 0..P-1
└── no other streams are legal during v1
```

The control stream sends the same 37-byte cookie, state bytes, and framed JSON
as TCP. Each data stream sends the same 37-byte cookie before payload. The
server classifies the first stream as control; after PARAM_EXCHANGE it accepts
exactly `parallel` additional streams during `CREATE_STREAMS`. An unexpected
unidirectional stream, extra bidirectional stream, wrong cookie, or early data
stream closes the connection with a netsu application error code.

### 7.3 TLS modes

QUIC encryption is mandatory. Authentication modes are explicit:

1. `--quic-ca <PEM>`: verify the server certificate against the provided CA.
2. `--quic-insecure`: accept a self-signed/untrusted server certificate and
   emit a warning to stderr. This mode is intended for benchmarks and tests.

The server accepts either:

1. `--quic-cert <PEM> --quic-key <PEM>`, which must be supplied together; or
2. `--quic-self-signed`, which generates one ephemeral certificate on startup.

There is no silent insecure default. `--quic` without a client trust mode or
without a server certificate mode fails before binding/dialing. Container E2E
uses self-signed plus explicit `--quic-insecure`. CI verifies CA-backed mode in
an integration test using a generated test CA.

0-RTT and session resumption are disabled so setup measurements always cover a
full handshake and payload is never replayable.

### 7.4 QUIC setup and transport metrics

The result records:

- DNS resolution duration, when a hostname is used.
- UDP endpoint creation duration.
- QUIC/TLS handshake duration.
- remote socket address.
- smoothed QUIC RTT at result time.
- lost packet count, congestion events, sent/received UDP datagrams, and sent/
  received UDP bytes when Quinn exposes them on the pinned version.
- certificate verification mode (`ca` or `insecure`).

Quinn transport statistics are diagnostic only. Application counters remain
the throughput authority.

### 7.5 QUIC timeouts and close behavior

- Address resolution/connect timeout: 10 seconds.
- QUIC handshake timeout: 10 seconds.
- Control read timeout: existing 30 seconds.
- Data-stream creation timeout: 10 seconds for the whole requested set.
- Graceful drain timeout after the duration deadline: 5 seconds.
- Endpoint shutdown wait: at most 2 seconds after sending a QUIC application
  close frame.

Timeouts must identify their phase in the error message. All tasks, streams,
connections, and endpoints are closed on success and error.

## 8. WebRTC DataChannel binding

### 8.1 Stack and role

- Native implementation: webrtc-rs `0.17.1`.
- Browser conformance implementation: Chromium's standard WebRTC API.
- Data only; no media engine codecs or RTP tracks are required.
- One `RTCPeerConnection` per test.
- The netsu client is the WebRTC offerer and DataChannel creator.
- The netsu server is the answerer and accepts remote-created DataChannels.
- DataChannels use DTLS/SCTP and are always binary.

### 8.2 DataChannel mapping

```text
RTCPeerConnection
├── label "netsu-control", protocol "netsu/iperf3-webrtc/1"
├── label "netsu-data/0", protocol "netsu/iperf3-webrtc/1"
├── label "netsu-data/1", protocol "netsu/iperf3-webrtc/1"
└── ... exactly P data channels
```

All v1 channels are:

```text
ordered = true
maxRetransmits = unset
maxPacketLifeTime = unset
negotiated = false
```

The control DataChannel is created before the offer. Data channels are created
only after the client receives `CREATE_STREAMS`. The server matches channels by
label, rejects duplicates, rejects unknown labels/protocols, and waits until all
requested channels reach `open` before sending `TEST_START`.

### 8.3 Message-to-byte adaptation

WebRTC DataChannels are message-oriented while netsu control framing is a byte
stream. `WebRtcPipe` therefore:

- sends each `write_all` as one or more binary messages;
- feeds received messages into a bounded byte queue;
- lets `read_exact(n)` span any number of DataChannel messages;
- rejects text messages;
- caps queued unread control bytes at 1 MiB;
- returns EOF only after close and after already-buffered bytes are consumed.

`WebRtcChannel` implements the bulk `DataChannel` trait. Large application
chunks are fragmented into binary messages no larger than 16 KiB. This v1 wire
cap is intentional: the pinned webrtc-rs 0.17.1 callback path documents a
16,384-byte receive limit, even when a browser advertises a larger SCTP maximum.
The receiver counts binary message payload bytes and does not need to
reconstruct the sender's original chunk boundary.

The v1 default application chunk is 64 KiB, normally emitted as four 16-KiB
messages. If either peer reports a lower maximum message size, the adapter
fragments further. A sender never sends a single SCTP message larger than either
16 KiB or the peer-advertised maximum. `-l` remains the application write size,
not necessarily the SCTP message size.

### 8.4 WebRTC backpressure

Every DataChannel uses event-driven buffering:

- high watermark: 4 MiB;
- low watermark: 1 MiB;
- pause application writes when buffered amount reaches the high watermark;
- resume only after the low-water event;
- never poll in a zero-delay busy loop;
- include time blocked on backpressure in per-stream diagnostics.

At test end the sender stops enqueuing, waits until buffered amount reaches
zero or the 5-second drain timeout expires, then closes the data channel.
Drain timeout is a test failure because it makes byte accounting ambiguous.

### 8.5 ICE configuration

The CLI accepts repeatable STUN server flags:

```text
--stun <stun:host:port>
```

Rules:

- No external ICE server is used by default.
- Host candidates alone support local/LAN and Docker direct tests.
- `--stun stun:stun.l.google.com:19302` is documented only as an opt-in manual
  public smoke example. CI never contacts it and netsu makes no availability or
  privacy promise for it.
- TURN URLs, credentials, and `iceTransportPolicy=relay` are not exposed by the
  CLI or library API in v1.
- The selected candidate pair must not contain a `relay` candidate. If it does,
  netsu closes the PeerConnection and fails the run before payload transfer.
- If host/server-reflexive/peer-reflexive candidates cannot connect before the
  ICE deadline, netsu prints a warning, returns a structured non-zero setup
  error, and emits no throughput result. There is no transparent relay fallback.

### 8.6 WebRTC setup and path metrics

The result records distinct monotonic durations:

- signaling WebSocket connection.
- room registration/join.
- offer creation and local-description application.
- ICE gathering.
- answer application.
- ICE connected.
- PeerConnection connected.
- all DataChannels open.
- total setup.

It also records the selected candidate pair:

- local candidate type (`host`, `srflx`, `prflx`, or `relay`).
- remote candidate type.
- ICE candidate protocol (`udp` or `tcp`).
- local and remote candidate addresses, redacted by default in human and JSON
  output unless `--include-addresses` is explicitly set.
- current transport RTT where the implementation exposes it.
- WebRTC/SCTP bytes and messages, when exposed.

The normalized path is `direct` or `unknown`. `unknown` is not allowed to enter
the payload phase. Any selected `relay` candidate is a policy error.

### 8.7 WebRTC timeouts

- Signaling connect: 10 seconds.
- Room register/join: 10 seconds.
- Offer/answer exchange: 15 seconds.
- ICE gathering: 15 seconds.
- ICE/PeerConnection connected: 20 seconds.
- All DataChannels open: 10 seconds after PeerConnection connected.
- Existing 30-second control-message timeout after channels are open.
- Graceful DataChannel drain: 5 seconds.
- PeerConnection close: 2 seconds.

Every timeout error names the phase and triggers signaling leave, DataChannel
close, PeerConnection close, and task shutdown.

## 9. Signaling service

### 9.1 Why it exists

WebRTC specifies peer connection behavior but not signaling. STUN discovers
public-facing addresses, but it does not exchange offer/answer/ICE messages.
The in-repository `apps/rendez-key` Worker already owns short-lived code
exchange, public routing, rate limiting, observability, Hono conventions, and
Cloudflare tests. netsu signaling is therefore added to the same Worker
deployment, under a separate route and state model. The Worker was migrated
from CrossCopy so a public netsu checkout is now sufficient for development,
CI, container E2E, and deployment.

This is a deployment merge, not a D1 schema merge. Existing immutable
`/v1/entries` store/claim behavior remains unchanged. Active WebSocket rooms
must never be modeled as D1 rows or kept in a Worker-global map.

### 9.2 Implementation and deployment

```text
cd /path/to/netsu
bun install
bun run signal:dev
netsu server --webrtc --signal-url http://127.0.0.1:8787/v1/signal
netsu client <ROOM_CODE> --webrtc --signal-url http://127.0.0.1:8787/v1/signal
```

The production implementation lives in netsu `apps/rendez-key/` and runs
only in the Cloudflare Workers runtime. Hono handles HTTP routing; a Durable
Object named from the normalized room code owns each room. WebSocket Hibernation
APIs own accepted sockets, so an idle room does not require an always-running
Worker isolate. It exposes:

```text
GET  /healthz                         existing liveness endpoint
POST /v1/signal/rooms                create a signaling room
GET  /v1/signal/rooms/:code/ws       WebSocket upgrade routed to that room DO
```

`wrangler dev`/workerd is the local runtime and is also used by Cloudflare's
Vitest pool. Bun may execute package scripts, but Bun is not the server runtime.
Container E2E starts Wrangler/workerd from this repository; it does not
maintain a second Bun WebSocket implementation.

For public tests the existing Worker custom domain can host the signaling
routes after its Durable Object migration is deployed. Both peers use the same
`https://.../v1/signal` base URL. The client derives `wss://` only for the room
WebSocket. Only small SDP/ICE JSON messages traverse the Worker; benchmark
payload remains peer-to-peer. A compatible self-hosted signaling URL remains
configurable.

This boundary is intentional:

- Rust and browser peers depend only on the versioned wire protocol.
- RendezKey code generation/normalization, Hono/OpenAPI conventions, rate
  limiting, observability, Wrangler config, and Cloudflare test harness are
  reused without coupling live room state to D1.
- One room code deterministically resolves with
  `env.SIGNAL_ROOMS.getByName(normalizedCode)` to one concurrency authority.
- netsu contains a minimal in-process test coordinator only for pure DataChannel
  tests. It is not deployable and is not treated as signaling conformance proof.

### 9.3 Protocol

Transport: WebSocket text frames containing one JSON object. Maximum frame size
is 64 KiB. Binary frames are rejected.

Room creation:

```http
POST /v1/signal/rooms
Content-Type: application/json

{"v":1,"ttl_seconds":600}
```

```json
{
  "v": 1,
  "code": "ABCD-EFGH",
  "listener_secret": "<high-entropy secret>",
  "expires_at": "..."
}
```

The secret is returned once, is never placed in a URL, and is never logged.
The listener proves possession in its first WebSocket message. The joiner needs
only the short room code. This prevents a code observer from replacing the
listener while preserving the intended short-code join flow.

Client-to-broker messages:

```json
{"v":1,"type":"bind","role":"listener","secret":"..."}
{"v":1,"type":"bind","role":"joiner"}
{"v":1,"type":"description","sdp_type":"offer","sdp":"..."}
{"v":1,"type":"description","sdp_type":"answer","sdp":"..."}
{"v":1,"type":"candidate","candidate":"...","sdp_mid":"0","sdp_mline_index":0,"username_fragment":"..."}
{"v":1,"type":"end_of_candidates"}
{"v":1,"type":"leave"}
```

Broker-to-client messages:

```json
{"v":1,"type":"bound","role":"listener","expires_in_seconds":600}
{"v":1,"type":"bound","role":"joiner","expires_in_seconds":600}
{"v":1,"type":"peer_ready"}
{"v":1,"type":"description","sdp_type":"offer","sdp":"..."}
{"v":1,"type":"description","sdp_type":"answer","sdp":"..."}
{"v":1,"type":"candidate","candidate":"...","sdp_mid":"0","sdp_mline_index":0,"username_fragment":"..."}
{"v":1,"type":"end_of_candidates"}
{"v":1,"type":"peer_left"}
{"v":1,"type":"error","code":"room_not_found","message":"room is unavailable"}
```

State rules:

1. `POST /rooms` generates an eight-character human-safe code, formatted
   `XXXX-XXXX`, initializes its Durable Object, and returns a listener secret.
2. The first WebSocket message must bind a role. The listener secret is checked
   in constant time; one joiner may claim the room. A second receives
   `room_full`.
3. After both sockets exist the broker sends `peer_ready` to each.
4. The joiner creates the control DataChannel and offer. The listener answers.
5. Description and candidate messages are forwarded only to the other room peer.
6. Messages invalid for the sender's role or current state receive an error and
   close code 1008.
7. Rooms expire after 600 seconds by default; accepted range is 60..3600. A DO
   alarm enforces expiry even after hibernation.
8. Disconnecting either peer sends `peer_left` to the other and deletes the
   room. Rooms are never reusable.

Resource limits:

- two sockets per room.
- 64 KiB per frame.
- 16 candidate messages per peer.
- 1 MiB total forwarded signaling bytes per room.
- room creation is protected by a dedicated Cloudflare rate-limit binding.
- the DO persists only lifecycle metadata, listener-secret hash, counters, and
  expiry. SDP/candidate payloads are forwarded, bounded, and not retained.

The Durable Object uses `ctx.acceptWebSocket()` plus `webSocketMessage`,
`webSocketClose`, and `webSocketError` handlers. Socket role metadata uses
WebSocket attachments so it survives hibernation. Alarm, close, and error paths
all converge on idempotent room cleanup.

The room code is a temporary bearer capability, not an identity or trust
mechanism. SDP and candidates contain network metadata and must not be logged at
normal verbosity.

## 10. Library API changes

Transport-specific settings must not turn `ClientOptions` and `ServerOptions`
into collections of unrelated nullable fields. Add nested options:

```rust
#[derive(Debug, Clone)]
pub struct QuicClientOptions {
    pub insecure: bool,
    pub ca_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct QuicServerOptions {
    pub self_signed: bool,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct WebRtcOptions {
    pub signal_url: String,
    pub stun_urls: Vec<String>,
    pub include_addresses: bool,
}

pub struct ClientOptions {
    // existing fields unchanged
    pub quic: Option<QuicClientOptions>,
    pub webrtc: Option<WebRtcOptions>,
}

pub struct ServerOptions {
    // existing fields unchanged
    pub quic: Option<QuicServerOptions>,
    pub webrtc: Option<WebRtcOptions>,
}
```

Validation requires that the nested option matching `transport` is present and
that unrelated nested options are absent. `Default` keeps both `None`.

## 11. Result model and JSON

Replace the iroh-only result field with a backward-compatible normalized model:

```rust
pub enum ConnectionInfo {
    Iroh(IrohConnectionInfo),
    Quic(QuicConnectionInfo),
    WebRtc(WebRtcConnectionInfo),
}

pub struct TestResult {
    // existing fields unchanged
    pub connection: Option<ConnectionInfo>,
}
```

The CLI's existing top-level `connection` JSON object remains optional. Existing
iroh key names remain accepted by snapshots. New examples:

```json
{
  "connection": {
    "transport": "quic",
    "path": "direct",
    "handshake_ms": 12.4,
    "rtt_us": 845,
    "remote_addr": "redacted",
    "certificate_verification": "ca",
    "stats": {
      "lost_packets": 0,
      "congestion_events": 0
    }
  }
}
```

```json
{
  "connection": {
    "transport": "webrtc",
    "path": "direct",
    "setup_ms": 83.2,
    "signaling_ms": 3.1,
    "ice_ms": 35.8,
    "data_channels_open_ms": 7.4,
    "rtt_us": 12500,
    "local_candidate_type": "srflx",
    "remote_candidate_type": "srflx",
    "ice_protocol": "udp",
    "addresses_included": false,
    "backpressure_blocked_ms": 4.2
  }
}
```

Durations are finite, non-negative numbers. Unknown diagnostics are omitted,
not emitted as zero. Listener secrets, SDP, and candidates are never serialized.

## 12. Error model

Add structured phase context without exposing dependency error internals as the
public contract:

```rust
pub enum SetupPhase {
    Resolve,
    Bind,
    Tls,
    QuicHandshake,
    SignalingConnect,
    SignalingRoom,
    OfferAnswer,
    IceGathering,
    IceConnected,
    PeerConnected,
    ChannelsOpen,
}

NetsuError::Setup {
    transport: &'static str,
    phase: SetupPhase,
    detail: String,
}
```

Public messages name transport and phase. They must not contain certificates,
private keys, SDP, or ICE candidate addresses unless verbose address output
was explicitly requested.

## 13. Automated testing strategy

### 13.1 Unit tests

Native QUIC:

- TLS option validation and CA verification.
- self-signed mode requires explicit insecure client trust.
- QUIC BytePipe exact reads across write boundaries.
- QUIC DataChannel payload accounting and EOF.
- wrong ALPN, wrong cookie, extra stream, early data stream, and timeout.
- connection statistics conversion does not invent unknown values.

WebRTC:

- Worker/DO signaling JSON validation and state-machine transitions in the
  in-repository Cloudflare test pool.
- room code normalization, creation collision retry, listener-secret
  authentication, hibernation restore, alarm expiry, room-full, disconnect
  cleanup, candidate limit, and frame-size limit.
- WebRtcPipe exact reads spanning messages and bounded-buffer rejection.
- binary-only enforcement.
- message fragmentation at configured SCTP maximum.
- high/low watermark backpressure without busy polling.
- unexpected label/protocol, duplicate control/data channel, and missing data
  channel timeout.
- address redaction and secret omission.

### 13.2 Local integration tests

Required matrix for each new transport:

| Direction | Parallel | Duration | Expected                |
| --------- | -------: | -------: | ----------------------- |
| upload    |        1 |       1s | success, non-zero bytes |
| reverse   |        1 |       1s | success, non-zero bytes |
| upload    |        4 |       1s | four stream results     |
| reverse   |        4 |       1s | four stream results     |

Additional lifecycle cases:

- two sequential tests succeed against one server.
- a concurrent second client receives server-busy.
- abrupt client disconnect releases server state.
- server close is bounded while setup is incomplete.
- unavailable peer or signaling service returns within the specified timeout.
- a network where direct ICE cannot connect emits the documented warning,
  returns non-zero, and produces no throughput result.
- forward and reverse results reconcile application bytes within 2% after
  graceful drain.

### 13.3 Browser interoperability

A Chromium fixture must implement the WebRTC binding independently using the
browser API. Required cells:

| Client   | Server   | Path                | Direction  | Parallel |
| -------- | -------- | ------------------- | ---------- | -------: |
| Chromium | netsu-rs | direct              | upload     |        1 |
| Chromium | netsu-rs | direct              | reverse    |        1 |
| Chromium | netsu-rs | direct              | upload     |        4 |
| Chromium | netsu-rs | blocked direct path | no payload |        1 |

The fixture speaks the real cookie/state/JSON protocol; an echo-only test does
not satisfy this requirement. Browser code must use `bufferedAmount` and
`bufferedamountlow`, send binary ArrayBuffers, and expose final JSON to the test
runner. The runner asserts non-zero bytes, state completion, channel count, and
path classification.

### 13.4 Container E2E

Create a separate extended transport harness so the existing fast
iperf3/TCP/UDP/WS matrix is not slowed or made dependent on Chromium:

```text
interop/transports/
├── Dockerfile.rs
├── docker-compose.yml
├── e2e.sh
├── run-matrix.ts
├── netem-entrypoint.sh
├── netem-profiles.json
└── browser/
    ├── Dockerfile
    ├── package.json
    ├── src/client.ts
    └── tests/webrtc.spec.ts
```

Services:

- `signal`: in-repository `apps/rendez-key` under Wrangler/workerd, with its real
  Durable Object binding and `/healthz`.
- `quic-server`: netsu-rs built with `quic,webrtc`.
- `webrtc-server`: same image, separate process.
- `client`: same image, driven by the matrix runner.
- `browser`: pinned Chromium/Playwright image.
- `direct-blocker`: a test network/profile that prevents all viable UDP paths,
  used only to assert bounded direct-connect failure.

Required container cells:

| Transport | Path/profile    | Direction           | Parallel | Peer         |
| --------- | --------------- | ------------------- | -------: | ------------ |
| QUIC      | bridge baseline | upload/reverse      |      1,4 | rs↔rs       |
| QUIC      | constrained     | upload              |        1 | rs↔rs       |
| QUIC      | lossy           | upload              |        1 | rs↔rs       |
| WebRTC    | ICE direct      | upload/reverse      |      1,4 | rs↔rs       |
| WebRTC    | ICE direct      | upload/reverse      |        1 | Chromium↔rs |
| WebRTC    | direct blocked  | fail before payload |        1 | rs↔rs       |
| WebRTC    | direct blocked  | fail before payload |        1 | Chromium↔rs |

`tc/netem` is applied only to the sending client's egress interface for these
profiles, preventing accidental double application of configured delay:

```json
{
  "baseline": {
    "rate": "500mbit",
    "delay": "10ms",
    "jitter": "0ms",
    "loss": "0%"
  },
  "constrained": {
    "rate": "100mbit",
    "delay": "50ms",
    "jitter": "5ms",
    "loss": "0.1%"
  },
  "lossy": { "rate": "100mbit", "delay": "20ms", "jitter": "0ms", "loss": "2%" }
}
```

Container assertions prove correctness, not speed:

- process exits zero before timeout.
- final output is parseable JSON.
- sent and received application bytes are non-zero.
- reliable-mode byte drift is at most 2% for rs↔rs and 5% for Chromium↔rs.
- stream count equals `parallel`.
- QUIC reports `transport=quic`, `path=direct`.
- successful WebRTC reports `path=direct`; `relay` or `unknown` fails the cell.
- blocked-direct cells return the documented setup exit code within the ICE
  deadline, contain the warning, and contain no throughput result.
- throughput is finite, positive, and below 1 Tbit/s; no minimum is asserted.
- no server/client/Worker/signaling processes or Compose resources remain.

On failure the harness writes JSON, process logs, selected candidate types, and
Compose logs under `interop/transports/results/`. It redacts SDP, candidate
addresses, listener secrets, and certificates.

### 13.5 CI gates

Existing CI remains unchanged until the new feature compiles locally. Then:

1. The Rust job adds feature-specific fmt/clippy/test commands for `quic` and
   `webrtc`, but the default build and `cargo publish --dry-run` remain gates.
2. `.github/workflows/e2e.yml` gains a separate `extended-transports` job with a
   45-minute timeout and log upload on failure.
3. `bun run e2e` continues to mean the existing core matrix.
4. `bun run e2e:transports` runs the new extended matrix.
5. `bun run e2e:all` runs core then extended matrices.

The extended job may be split by QUIC and WebRTC if measured CI wall time
exceeds 30 minutes, but neither matrix may be silently skipped.

## 14. Manual real-network validation

Containers validate protocol behavior and controlled impairment, not real NAT
or Wi-Fi behavior. Before release, record but do not CI-gate:

1. Two physical devices on one LAN: QUIC and WebRTC direct.
2. Two devices on different public networks using an explicitly configured
   STUN server: WebRTC server-reflexive direct path when available.
3. At least one restrictive network where direct ICE fails: verify the warning,
   bounded exit, and absence of a misleading throughput result.

For each run capture setup phases, selected path, RTT, parallel count, chunk
size, application throughput, byte reconciliation, operating systems, and
whether Wi-Fi or Ethernet was used. Do not present Docker throughput as LAN or
Internet performance evidence.

## 15. Documentation requirements

Update:

- root `README.md`: supported transports and quick commands.
- `netsu-rs/README.md`: feature flags, TLS modes, signaling deployment, STUN
  versus TURN explanation, direct-only policy, and failure behavior.
- `PROTOCOL.md`: QUIC and WebRTC bindings plus signaling protocol reference.
- `interop/README.md`: distinction between core and extended matrices.
- `interop/transports/README.md`: topology, commands, assertions, artifacts,
  and debugging individual cells.
- `apps/rendez-key/README.md`: signaling wire scope, Worker/DO local
  commands, migration, limits, security boundary, and public smoke procedure.

Every example must mark Google STUN as optional/manual and must not contain a
real listener secret. Documentation must state that signaling does not relay
benchmark payload, TURN is unsupported, and restrictive networks may fail.

## 16. Compatibility and rollout

- No default feature changes.
- Existing TCP/UDP/WS/iroh CLI forms and JSON remain valid.
- New enum variants are compile-gated like existing optional transports.
- Existing iroh JSON is migrated through snapshot tests before deleting the
  old `iroh_connection` field.
- Cargo release size checks record default, `quic`, `webrtc`, and combined
  binary sizes; only the default binary remains subject to the current small
  binary expectation.
- QUIC and WebRTC each land in independently reviewable commits and must pass
  their own plan before the combined extended matrix becomes required.

## 17. Acceptance criteria

Native QUIC is accepted when:

- all four local direction/parallel cells pass;
- CA verification and explicit insecure mode are both tested;
- handshake/stream/lifecycle failure tests are bounded;
- baseline/constrained/lossy Docker cells pass;
- JSON reports a direct QUIC path and finite diagnostics;
- default-feature tests and the existing core Docker matrix remain green.

WebRTC is accepted when:

- signaling unit/integration tests cover state, expiry, cleanup, limits, and
  malformed peers;
- all four Rust local direction/parallel cells pass;
- direct Docker cells pass with proven path classification;
- blocked-direct Docker cells fail before payload within the deadline;
- required Chromium interoperability cells pass;
- backpressure, message fragmentation, drain, timeout, redaction, and secret
  omission tests pass;
- no automated test contacts Google STUN or any external signaling host;
- default-feature tests and the existing core Docker matrix remain green.

The combined feature is accepted when `cargo fmt --check`, all relevant clippy
and test feature matrices, `cargo publish --dry-run`, `bun run e2e`, and
`bun run e2e:transports` all exit zero from a clean checkout.

## 18. Known risks and mitigations

| Risk                                                | Mitigation                                                                                   |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| webrtc-rs 0.17 is maintenance-only                  | Pin exactly, isolate behind adapters, and require Chromium E2E.                              |
| QUIC certificate shortcuts become insecure defaults | Require explicit CA or `--quic-insecure`; never silently skip verification.                  |
| WebRTC silently measures a relay path               | Do not configure TURN, inspect the selected pair, reject relay/unknown before payload.       |
| DataChannel buffering inflates sent bytes           | Event-driven watermarks, graceful drain, and sender/receiver reconciliation.                 |
| Container test reports are mistaken for benchmarks  | Assert correctness only and label netem evidence as controlled.                              |
| Public signaling abuse                              | Dedicated creation rate limit, short TTL, one room DO, strict frame/candidate/byte caps.     |
| Secrets leak through diagnostics                    | One-time listener secret, hashed DO storage, redaction tests, no SDP/candidate normal logs.  |
| Worker and transport protocol drift                 | Keep both in one netsu revision and run the Worker conformance suite before transport cells. |
| Existing user TUI work conflicts                    | Do not modify `netsu-rs/src/tui.rs` in either implementation plan.                           |

## 19. Source notes

- Quinn current API: <https://docs.rs/quinn/latest/quinn/>
- WebRTC standard: <https://www.w3.org/TR/webrtc/>
- WebRTC peer connection example with optional Google STUN:
  <https://webrtc.org/getting-started/peer-connections?hl=en>
- webrtc-rs pinned maintenance release:
  <https://docs.rs/webrtc/0.17.1/webrtc/>
- Cloudflare Durable Object WebSocket server and hibernation guidance:
  <https://developers.cloudflare.com/durable-objects/examples/websocket-server/>
- Cloudflare Durable Object testing:
  <https://developers.cloudflare.com/durable-objects/examples/testing-with-durable-objects/>
- Cloudflare Realtime TURN is a separate managed relay product, not a Worker
  feature and not part of this scope:
  <https://developers.cloudflare.com/realtime/turn/>
