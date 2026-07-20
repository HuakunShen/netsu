# netsu

An iperf3-compatible network speed test — TypeScript library and CLI, with a
Rust implementation planned for a later phase.

## Layout

- [`packages/netsu`](./packages/netsu) — the TypeScript package (library +
  CLI), published to npm as `netsu` and to JSR as `@hk/netsu`. Start here.
- [`PROTOCOL.md`](./PROTOCOL.md) — the wire-protocol reference (iperf3
  compatibility + the netsu WebSocket extension); the authority both
  implementations are built against.
- `netsu-rs` — a Rust implementation of the same protocol, planned for
  Phase 2/3 (Docker-based cross-implementation interop testing included).

See [`packages/netsu/README.md`](./packages/netsu/README.md) for install and
usage instructions.
