# System Overview

**Updated: 2026-07-22** — Established the initial RepoWiki baseline from the
current source, protocol reference, and recent Rust/TUI changes.

netsu is an iperf3-compatible network speed-test project with TypeScript and
Rust implementations. Both implement the same control and data protocol, and
are verified against each other and the official `iperf3` binary for TCP and
UDP. WebSocket is a netsu-only extension between netsu peers.

## Components

| Component | Responsibility |
| --- | --- |
| `packages/netsu` | TypeScript library and CLI, published as `netsu` and `@hk/netsu`. |
| `netsu-rs` | Rust library and CLI; optional iroh, mux, TUI, and input-demo capabilities. |
| `PROTOCOL.md` | Normative wire-protocol reference shared by both implementations. |
| `interop/` | Docker-based compatibility matrix across TypeScript, Rust, and official iperf3. |

## Transport boundary

TCP and UDP retain iperf3 compatibility in both directions. WebSocket carries
the same byte stream inside binary frames but is only available between netsu
peers. The Rust `iroh` feature is separate from the iperf3 transport set: it
runs the same test protocol over an iroh/QUIC connection and uses short
rendez-key codes for peer discovery.

## Verification boundary

`bun run e2e` runs the interop matrix. It checks TCP and WebSocket peer byte
agreement within its configured tolerance, while UDP checks transfer/rate
plausibility because packet loss is valid. The matrix is compatibility evidence,
not a throughput benchmark.

## Related pages

- [Protocol and Interoperability](Architecture/Protocol%20and%20Interoperability.md)
- [Rust Implementation](Services/Rust%20Implementation.md)
- [Cross-device TUI](Services/Cross-device%20TUI.md)
- [Multiplexing Lab](Services/Multiplexing%20Lab.md)
