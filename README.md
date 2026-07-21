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
- [`apps/rendez-key`](./apps/rendez-key) — the open-source Cloudflare Worker
  control plane for temporary test codes and, in the planned WebRTC transport,
  short-lived signaling. Benchmark payloads never pass through this service.

## Speaking the protocol

Both implementations interoperate with real `iperf3` over **tcp** and **udp**,
in both directions, plus a netsu-only **ws** transport between netsu peers.
Conformance is enforced by the interop matrix:

```sh
bun run e2e
```

See each package's README for install and usage:
[TypeScript](./packages/netsu/README.md) · [Rust](./netsu-rs/README.md).

## RendezKey development

RendezKey runs on Cloudflare Workers/D1. Bun manages the monorepo and invokes
Wrangler; it does not replace the Workers runtime:

```sh
bun install
bun run signal:dev
bun run signal:typecheck
bun run signal:test
bun run signal:deploy:dry
```

Anonymous creation, when enabled, is limited separately from code lifetime and
claim count. The default limiter is 10 creates per 60 seconds per IP per
Cloudflare location; automated tests should use local Wrangler/workerd or a
protected token instead of load-testing the public anonymous endpoint.
