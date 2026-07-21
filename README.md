# netsu

An iperf3-compatible network speed test, in two implementations that speak the
same wire protocol and interoperate with the official `iperf3` binary.

## Layout

- [`packages/netsu`](./packages/netsu) — the **TypeScript** package (library +
  CLI), published to npm as `netsu` and to JSR as `@hk/netsu`.
- [`netsu-rs`](./netsu-rs) — the **Rust** implementation of the same protocol
  (library + CLI), installable via `cargo install netsu`.
- [`PROTOCOL.md`](./PROTOCOL.md) — the wire-protocol reference (iperf3
  compatibility + the netsu WebSocket extension); the authority both
  implementations are built against.
- [`interop/`](./interop) — the Docker-based cross-implementation interop
  matrix: every client × server × transport × direction across netsu-ts,
  netsu-rs, and official iperf3.

## Speaking the protocol

Both implementations interoperate with real `iperf3` over **tcp** and **udp**,
in both directions, plus a netsu-only **ws** transport between netsu peers.
Conformance is enforced by the interop matrix:

```sh
bun run e2e
```

See each package's README for install and usage:
[TypeScript](./packages/netsu/README.md) · [Rust](./netsu-rs/README.md).
