# Extended transport E2E

This directory is the isolated correctness harness for transports that need
extra runtime support. It deliberately stays separate from the fast
TCP/UDP/WebSocket/iperf3 matrix in `interop/`.

## Native QUIC matrix

Run all six rs-to-rs cells:

```bash
bun run e2e:quic
```

The wrapper builds `netsu-rs` with the `quic` feature inside Docker, starts one
server and one client on a private bridge, applies a validated `tc/netem`
profile only inside the client container, and always tears the Compose project
down. The cells cover upload/reverse with one and four streams plus constrained
and lossy upload profiles.

To repeat one cell while debugging:

```bash
QUIC_CASE=lossy-upload-p1 bun run e2e:quic
```

These are protocol-correctness checks, not performance benchmarks. They assert
successful completion, JSON shape, byte agreement, stream count, bounded
handshake, and a direct QUIC path. They intentionally do not require a minimum
throughput. On failure, redacted process output is written under `results/`;
Compose logs are added as `results/compose.log`.

The netem entrypoint accepts only named profiles from
`netem-profiles.json`. Every rate, time, and loss value is checked against an
anchored unit-bearing expression before it reaches `tc`.

## Direct-only WebRTC matrix

Run all nine Worker/Rust/Chromium cells:

```bash
bun run e2e:webrtc
```

The wrapper builds three self-contained images, starts RendezKey with
Wrangler/workerd, waits for `/healthz`, runs the matrix, and always removes the
Compose project. Bun installs dependencies and drives the client-side runner;
it is not the signaling server runtime. The Worker, Rust binary, and Chromium
fixture all come from the same checkout.

The local Compose stack injects `netsu-compose-test-token` into the Worker and
Rust server. That privileged local tier bypasses the public anonymous creation
limiter, so a large CI matrix does not consume a 10-rooms-per-60-seconds public
allowance. The token is a non-secret container fixture and is not embedded in
an image. Automated tests never contact Google STUN or a public signaling URL.

The cells cover:

- Rust upload/reverse with one and four payload DataChannels;
- Chromium upload with one/four channels and reverse with one channel;
- Rust and Chromium clients with UDP to the peer rejected, proving bounded
  direct-path failure, exit code 4, the exact warning, and no throughput fields.

To debug one cell against an already-started stack:

```bash
COMPOSE_PROJECT_NAME=netsu-webrtc-debug \
  docker compose -f interop/transports/docker-compose.yml up -d --wait
COMPOSE_PROJECT_NAME=netsu-webrtc-debug \
  WEBRTC_CASE=chromium-reverse-p1 \
  bun interop/transports/run-webrtc-matrix.ts
COMPOSE_PROJECT_NAME=netsu-webrtc-debug \
  docker compose -f interop/transports/docker-compose.yml down -v
```

Success requires a proven `host`, `srflx`, or `prflx` direct candidate pair,
non-zero reconciled application bytes, the requested stream count, and finite
positive throughput below 1 Tbit/s. No minimum rate is asserted. Failure
artifacts are written to `results/` and redact listener secrets, SDP,
candidates, and addresses before CI uploads them.

These cells are controlled protocol-correctness evidence. They do not prove a
public-network NAT path. The
[manual release smoke](../../docs/release/webrtc-public-smoke.md) uses two
physical networks and an explicitly configured STUN service; restrictive-
network failure is an expected supported result because TURN is intentionally
absent.
