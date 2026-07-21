# Rust Implementation

**Updated: 2026-07-22**

`netsu-rs` provides the Rust library and CLI for the shared iperf3-compatible
core. The lean default build contains TCP and UDP; optional capabilities remain
behind Cargo features so non-core transports and UI do not enlarge the default
binary.

## Feature boundaries

| Feature | Adds |
| --- | --- |
| `ws` | netsu-only WebSocket transport. |
| `iroh` | iroh/QUIC test transport, rendez-key codes, and `netsu mux`. |
| `tui` | Ratatui launcher and live dashboard. |
| `input-demo` | Keyboard/mouse-sharing demo; implies `iroh`. |

The iroh client accepts either a full endpoint ticket or a short rendez-key
code. Normal iroh mode supports NAT traversal and relay fallback; `--direct-only`
requires a direct path and is appropriate only where the server is reachable
directly.

## Recent implementation note: bounded WebSocket teardown

**Fixed: 2026-07-22** — The Rust WebSocket transport now bounds its closing
handshake to 500 ms. In forward-mode tests, a pure sender can stop reading once
its duration expires and never return the peer Close frame. An unbounded close
on the receiver would then hold the server's single-test lock and leave later
clients receiving `server busy`.

`WsPipe` and `WsDataChannel` now time out the graceful close and drop the stream
when necessary. The protocol sees the TCP end-of-stream, while teardown cannot
deadlock the server. The fix was verified in the Docker interop environment
that reproduced the problem.

## Related pages

- [Protocol and Interoperability](../Architecture/Protocol%20and%20Interoperability.md)
- [Cross-device TUI](Cross-device%20TUI.md)
- [Multiplexing Lab](Multiplexing%20Lab.md)
