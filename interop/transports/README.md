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
