# Native QUIC and WebRTC TUI Integration

**Status:** Implemented

**Date:** 2026-07-22

## 1. Objective

Expose netsu's fixed-address native QUIC and direct-only WebRTC transports in
the existing cross-device Ratatui Host/Join workflow. A user must be able to
run both transports without memorizing CLI flags while the TUI preserves the
same transport, trust, signaling, privacy, and failure semantics as the CLI.

This work extends the current uncommitted TUI host/join implementation. It must
preserve its direct `host:port` flow for socket transports and its rendez-key
flow for iroh.

## 2. Transport semantics

### 2.1 Native QUIC

- The mode is labelled `Native QUIC`, distinct from `iroh / QUIC`.
- The host binds the port from the editable advertised `host:port` value and
  uses `QuicServerOptions { self_signed: true, cert_path: None, key_path: None }`.
- The joiner dials the entered `host:port` with
  `QuicClientOptions { insecure: true, ca_path: None }`.
- Both configuration and summary screens state that certificate verification
  is disabled for this benchmark convenience mode.
- Authenticated `--quic-cert`, `--quic-key`, and `--quic-ca` operation remains
  CLI-only. The TUI must not pretend its self-signed mode authenticates a peer.
- QUIC remains reliable, so the TUI never combines it with UDP mode.

### 2.2 WebRTC

- The mode is labelled `WebRTC`, with a direct-only hint.
- The host creates a short-lived signaling room and displays its room code.
- The joiner enters the room code and uses the same signaling and STUN
  configuration as the host.
- Payload DataChannels remain peer-to-peer. The signaling Worker carries only
  short-lived room metadata, SDP, and ICE candidates.
- Only host, server-reflexive, and peer-reflexive selected paths are accepted.
- TURN URLs and relay candidates remain unsupported. If direct ICE fails, the
  TUI shows the stable direct-connection warning and no throughput result.

## 3. Defaults and environment variables

The TUI initializes editable WebRTC fields as follows:

| Setting | Environment variable | Unset default |
| --- | --- | --- |
| Signaling base URL | `NETSU_SIGNAL_URL` | `https://rendez-key.xc.huakun.tech/v1/signal` |
| STUN URL list | `NETSU_STUN_URLS` | `stun:stun.cloudflare.com:3478` |
| Signaling bearer token | existing `NETSU_SIGNAL_TOKEN` | no token |

`NETSU_STUN_URLS` is a comma-separated list of zero to four `stun:` URLs. An
explicit empty value disables STUN, which lets local development and automated
tests avoid all public network dependencies. The editable TUI field uses the
same comma-separated representation.

The signaling URL must be an absolute HTTP(S) URL. STUN validation delegates to
`WebRtcOptions::new`, which also rejects non-STUN ICE URLs and more than four
entries. Secrets are never displayed or edited in the TUI.

Cloudflare's public STUN service helps discover a direct public mapping; it is
not a relay. Cloudflare Realtime TURN is deliberately not configured.

## 4. Feature boundaries

- `tui` remains the only feature required for the local loopback lab.
- Cross-device Host/Join screens compile when any of `iroh`, `quic`, or
  `webrtc` is enabled.
- Each optional mode appears only when its Cargo feature is enabled.
- TCP and UDP remain available within every cross-device build. WebSocket
  remains conditional on `ws`.
- Rendez-key imports and ticket store/claim calls remain conditional on `iroh`;
  QUIC and WebRTC builds must not acquire an iroh dependency.
- `cargo check --features tui,quic,webrtc` must work without `iroh`.

## 5. Interaction design

The existing HostConfig and JoinConfig screens become contextual forms rather
than separate per-transport wizards.

### 5.1 Shared navigation

- `Tab` and `Shift+Tab` move focus among fields valid for the selected mode.
- A focused panel uses the existing semantic accent color on its border.
- `Enter` starts hosting or joining only after synchronous validation succeeds.
- `Esc` returns home without starting network work.
- Editing any field clears its previous validation error.

### 5.2 Host form

- Transport focus: `Up/Down` changes the mode.
- Address focus exists for TCP, UDP, WebSocket, and Native QUIC and edits
  `host:port`.
- WebRTC replaces the address field with editable signaling URL and STUN list
  fields.
- iroh has no transport-specific input.
- The bottom status line shows validation errors without leaving the form.

### 5.3 Join form

- Transport focus: `Up/Down` changes the mode.
- Target focus edits `host:port` for socket/Native QUIC, or a room/code value
  for iroh/WebRTC.
- WebRTC additionally exposes signaling URL and STUN list fields.
- Options focus uses `Left/Right` for duration and `Space` for reverse mode.
- The help bar changes labels with focus and mode.

### 5.4 Hosting and summary

- TCP, UDP, WebSocket, and Native QUIC display the dialable address.
- iroh and WebRTC display a short shareable code.
- Hosting identifies the selected transport and direct/benchmark security mode.
- Existing interval, sparkline, completion, stop, and summary behavior is
  reused for both new transports.
- CLI reproduction text contains the exact `--quic` or `--webrtc` flags and
  public, non-secret configuration values.

## 6. Data flow and implementation boundaries

TUI state owns only editable strings, focus, and selected modes. Pure helper
functions parse `host:port`, split STUN URLs, validate WebRTC options, describe
field sets, and build CLI reproduction strings. Tests exercise these helpers
without starting network services.

`spawn_host` maps the selected mode into `ServerOptions`:

- Native QUIC selects `Transport::Quic` and supplies self-signed options.
- WebRTC selects `Transport::WebRtc`, supplies validated `WebRtcOptions`, uses
  port zero, and reads the room code from `NetsuServer::endpoint_ticket`.

`spawn_join` maps the selected mode into `ClientOptions`:

- Native QUIC selects `Transport::Quic` and supplies insecure trust options.
- WebRTC selects `Transport::WebRtc`, supplies validated `WebRtcOptions`, and
  passes the room code as the client host argument.

No transport behavior is reimplemented in `tui.rs`; it calls the same library
entry points as the CLI.

## 7. Error handling

- Invalid address, signaling URL, or STUN list fails synchronously in the form.
- Server startup, signaling, ICE, TLS, and transport errors become failed TUI
  summaries and never panic the event loop.
- WebRTC direct-path failure uses the same warning vocabulary as the CLI and
  never emits a successful Mbps result.
- QUIC insecure mode always remains visible in configuration and result copy.
- Stopping a host closes `NetsuServer` and releases its port/room resources.

## 8. Testing and acceptance

### Headless TUI tests

- Feature-dependent mode lists include Native QUIC and WebRTC exactly when
  compiled.
- Host and Join forms render the correct fields, warnings, focus border, and
  help text for each new mode.
- Key handling cycles focus, edits fields, changes duration/reverse, and blocks
  invalid submissions.
- Environment defaults, explicit overrides, empty STUN disablement, comma
  splitting, validation, and CLI reproduction strings are deterministic.
- Pure option mapping produces the expected QUIC and WebRTC library options.

### Rust integration

- `cargo test --features tui,quic,webrtc` passes.
- `cargo test --features tui,iroh,quic,webrtc,ws` passes.
- Existing QUIC and WebRTC transport, CLI, workerd signaling, and direct-path
  tests remain green.
- Clippy with all relevant features has no warnings.

### External and container gates

- Automated WebRTC tests use local Wrangler/workerd and explicitly disable
  public STUN/signaling defaults.
- Existing `bun run e2e:quic` and `bun run e2e:webrtc` container matrices pass.
- A manual PTY smoke confirms both modes are selectable and their contextual
  fields fit a representative 100x30 terminal.
- No automated test contacts `rendez-key.xc.huakun.tech` or Cloudflare STUN.

## 9. Non-goals

- TURN or any relay fallback.
- Authenticated certificate file selection in the TUI.
- Persisting TUI settings to disk.
- Starting Wrangler automatically from the TUI.
- Changing transport wire protocols, WebRTC signaling v1, or container
  performance thresholds.
