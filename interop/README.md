# netsu interop matrix

Proves the three implementations — `netsu-ts`, `netsu-rs`, and official
`iperf3` — actually talk to each other, by running every
client × server × transport × direction combination and asserting the two sides
agree on how much data crossed the wire.

What it catches that unit tests can't: a protocol divergence between two
*independent* implementations. That shows up as the two sides disagreeing on
byte counts, or as a cell that can't complete at all.

## What it proves (and doesn't)

- **TCP/WS cells** assert both sides report the same bytes transferred (within
  2%). iperf3's whole premise is that the receiver's count is authoritative;
  a larger disagreement means a protocol bug, not a network effect.
- **UDP cells** assert non-zero transfer and a plausible rate only — UDP
  legitimately loses packets, so byte agreement isn't required.
- Speeds are checked for sanity (> 0, < 1 Tbit/s), never for an absolute
  number: a container-to-container figure on a shared runner is not a
  benchmark, and asserting throughput would make the matrix flaky.

## Prerequisites

- **Docker** (Docker Desktop, OrbStack, Colima, or a plain daemon — anything
  that provides `docker compose`).
- **bun** (drives the runner).
- **Rust toolchain** with the host-arch musl target, and **`cross`** (or, on
  Linux, `musl-tools`) to cross-compile the static `netsu-rs` binary the Rust
  container runs:

  ```sh
  rustup target add aarch64-unknown-linux-musl   # or x86_64-... on Linux
  cargo install cross                            # macOS hosts
  ```

## Run it

One command from the repo root:

```sh
bun run e2e
```

That cross-compiles `netsu-rs` to a static musl binary
(`interop/build-rust.sh` → `interop/bin/netsu-rs-<arch>`, gitignored), builds
the three images, brings up the network, runs the matrix, and tears everything
down. CI runs the exact same script (`.github/workflows/e2e.yml`).

## Which cells are skipped, and why

- **iperf3 × iperf3** — proves nothing about netsu; it's the control case a
  human runs by hand when a result looks suspicious.
- **any WS cell involving iperf3** — official iperf3 can't speak netsu's
  WebSocket extension. Expected, not a failure.

Every other cell runs and must pass; each skip is printed with its reason so a
silently-dropped cell can't masquerade as coverage.

## Debugging one cell by hand

The containers idle (`sleep infinity`) and the runner drives them with
`docker compose exec`, so you can reproduce any cell manually. Bring the stack
up, then exec a server and a client:

```sh
export NETSU_RS_BIN="interop/bin/netsu-rs-$(uname -m | sed 's/arm64/aarch64/')"
docker compose -f interop/docker-compose.yml up -d

# e.g. netsu-rs client -> netsu-ts server, TCP:
docker compose -f interop/docker-compose.yml exec -T netsu-ts \
  bun dist/cli.mjs server -p 5401 &
docker compose -f interop/docker-compose.yml exec -T netsu-rs \
  /usr/local/bin/netsu client netsu-ts -p 5401 -t 3 --json

docker compose -f interop/docker-compose.yml down -v
```

Service-name DNS (`netsu-ts` / `netsu-rs` / `iperf3`) is provided by compose;
that's why the client addresses its peer by service name.

## A note on the runner

`run-matrix.ts` drives the containers via `docker compose exec`. A Docker-SDK
variant (`@docker/node-sdk` for the per-cell exec loop, giving real exit codes
and clean stdout/stderr demux instead of string-parsing) is a future
optimization; it's gated on a small spike to confirm the SDK demultiplexes
non-TTY streams and reaches the same daemon as compose (set `DOCKER_HOST` once
and pass it to both, or they can pick different daemons). The CLI path here is
the robust fallback and needs no extra dependency.
