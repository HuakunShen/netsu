# netsu (Rust)

An iperf3-compatible network speed test — a library and a CLI.

netsu speaks **iperf3's wire protocol**, so it interoperates with the official
`iperf3` binary in both directions (netsu client ↔ iperf3 server, iperf3 client
↔ netsu server) over TCP and UDP.

Beyond the iperf3 core, netsu has optional, opt-in capabilities behind cargo
features (see [Optional features](#optional-features)): a WebSocket transport,
an authenticated fixed-address **native QUIC transport**, an **iroh/QUIC
transport** with short shareable codes, a **multiplexing +
priority latency lab** (`netsu mux`), an interactive **cross-device TUI**
(host/join two machines by sharing a code), and a keyboard/mouse sharing **demo**.

This is the Rust implementation. A matching TypeScript implementation lives in
[`packages/netsu`](../packages/netsu); the two are protocol-compatible and
share the wire-protocol spec in [`../PROTOCOL.md`](../PROTOCOL.md).

## Install

```sh
cargo install netsu                          # lean TCP/UDP core (~1 MB binary)
cargo install netsu --features quic          # + fixed-address native QUIC
cargo install netsu --features webrtc        # + direct-only WebRTC DataChannels
cargo install netsu --features iroh,tui      # + iroh transport, mux lab, TUI
```

The default build is the smallest possible iperf3-compatible TCP+UDP tool
(~980 KB, stripped, LTO). Every non-core transport/UI is opt-in so it only adds
binary size when you enable it — see [Optional features](#optional-features).
(A Rust tokio/clap binary can't reach iperf3's 192 KB C footprint; `std + tokio

- clap` is a ~1 MB floor.)

## CLI

```
netsu server [-p 5201] [--ws | --quic | --webrtc]
netsu client <host|room-code> [-p 5201] [-u | --ws | --quic | --webrtc] [-t 10] [-P 1] [-R] [-b 1M] [-l 128K] [-i 1] [--json]
```

Flag semantics mirror iperf3: `-R` means the server sends (download), `-b`
takes decimal K/M/G rates (`1M` = 1,000,000 bits/s), `-l` takes a 1024-based
block size (`128K` = 131,072 bytes), and `--json` emits an iperf3-aligned
structure.

Start a server, then run a client against it:

```sh
# terminal 1
netsu server -p 5201

# terminal 2 — a 5-second TCP upload with 4 parallel streams
netsu client 127.0.0.1 -p 5201 -t 5 -P 4

# a reverse (download) UDP test, paced at 5 Mbit/s, as JSON
netsu client 127.0.0.1 -p 5201 -u -b 5M -R --json
```

netsu also talks to a real iperf3 on the other end:

```sh
iperf3 -s -p 5201                  # official iperf3 server
netsu client 127.0.0.1 -p 5201 -t 5
```

## Library

```rust
use netsu::client::{run_client, ClientOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = run_client(
        "127.0.0.1",
        ClientOptions { port: 5201, duration: 5, ..Default::default() },
        None, // optional per-interval callback: Some(Box::new(|report| { ... }))
    )
    .await?;

    println!(
        "sent {} bytes at {:.1} Mbit/s",
        result.sent_bytes,
        result.send_bits_per_second / 1e6,
    );
    Ok(())
}
```

`ClientOptions` covers transport (`Tcp`/`Ws`/`Quic`/`WebRtc`/`Iroh`, subject to
enabled features), `udp`, `reverse`, `duration`, `parallel`, `len`, `bandwidth`,
and an `interval` callback; `TestResult` carries
both sides' byte counts, per-stream results, and (for UDP) jitter/loss.

Run a server from the library with `netsu::server::start_server(ServerOptions {
.. })`, which returns a handle whose `close()` releases the port.

## Protocol

The wire protocol is documented in [`../PROTOCOL.md`](../PROTOCOL.md) and is the
single source of truth shared by both implementations. Conformance is checked
against the real `iperf3` binary in this crate's integration tests.

## Optional features

| Feature      | Adds                                                                   | Extra deps                     |
| ------------ | ---------------------------------------------------------------------- | ------------------------------ |
| `ws`         | WebSocket transport (`--ws`), HTTP-proxy traversable                   | tokio-tungstenite              |
| `quic`       | Fixed-address native QUIC transport (`--quic`) with explicit TLS trust | Quinn, rustls, rcgen           |
| `webrtc`     | Direct-only WebRTC DataChannels (`--webrtc`) + signaling client        | webrtc-rs, reqwest             |
| `iroh`       | iroh/QUIC transport (`--iroh`) + `netsu mux` lab + rendez-key codes    | iroh, reqwest, hdrhistogram, … |
| `tui`        | `netsu tui` — cross-device host/join launcher + live dashboard         | ratatui                        |
| `input-demo` | `examples/kbm-demo` keyboard/mouse sharing (implies `iroh`)            | monio                          |

### Native QUIC transport (`--features quic`)

Native QUIC keeps ordinary `host:port` addressing and multiplexes one control
stream plus exactly `-P` payload streams over one Quinn connection:

```sh
# terminal 1
cargo run --features quic -- server --quic --quic-self-signed -p 5201

# terminal 2
cargo run --features quic -- client 127.0.0.1 \
  --quic --quic-insecure -p 5201 -t 10 -P 4
```

The server must choose `--quic-self-signed` or both `--quic-cert` and
`--quic-key`. The client must choose `--quic-ca` or the explicit test-only
`--quic-insecure` mode, which prints a warning. 0-RTT/resumption are disabled.
This transport does not perform NAT traversal or relay fallback, and official
iperf3 cannot speak it. Its ALPN is `netsu/iperf3-quic/1`.

Run `bun run e2e:quic` from the repository root for the isolated Docker/netem
correctness matrix. Its reported speeds are not LAN benchmark results.

### Direct-only WebRTC (`--features webrtc`)

WebRTC reuses the normal netsu control and result protocol over one reliable,
ordered control DataChannel and exactly `-P` reliable, ordered payload
DataChannels. A short-lived room in `apps/rendez-key` exchanges signaling only;
benchmark payloads never traverse the Worker.

```sh
# terminal 1, repository root: real local Cloudflare runtime
./scripts/dev-webrtc-signal.sh

# terminal 2: creates a 10-minute room and prints its code
cd netsu-rs
cargo run --features webrtc -- server --webrtc \
  --signal-url http://127.0.0.1:18787/v1/signal

# terminal 3: upload, reverse with -R, or use -P for parallel channels
cd netsu-rs
cargo run --features webrtc -- client <ROOM_CODE> --webrtc \
  --signal-url http://127.0.0.1:18787/v1/signal -t 10 -P 4
```

No external ICE server is used by default, which is enough for local/LAN and
container tests. For a manual Internet smoke you may opt into Google's public
STUN service on both commands with
`--stun stun:stun.l.google.com:19302`. It is an external service: netsu makes
no availability, privacy, quota, or cost guarantee, and automated tests never
contact it.

Before a release, use the repository's
[two-network public smoke procedure](../docs/release/webrtc-public-smoke.md) to
verify both a successful direct path and the bounded restrictive-network
failure without collecting addresses, SDP, or raw candidates.

TURN URLs and relay flags are rejected. If direct ICE cannot connect, netsu
prints a warning, exits with code 4, and does not report zero as though it were
a completed throughput test. Configuration errors use exit 2 and bounded
signaling/setup timeouts use exit 3. `--json` emits a structured `error` object
without `bits_per_second`. Candidate addresses are redacted unless
`--include-addresses` is explicitly supplied.

### iroh transport (`--features iroh`)

Runs the same iperf3 throughput/latency test over one iroh/QUIC connection
(control + data streams multiplexed), with NAT traversal and a shareable short
code instead of `host:port`:

```sh
netsu server --iroh --direct-only        # prints an ~8-char code + a full ticket
netsu client <code|ticket> --iroh -t 10 -P 4 -R --json
```

The server publishes a **rendez-key** short code alongside the full ticket; the
client's peer argument accepts either — a short code is claimed automatically, a
long ticket is used directly (told apart by length). Publishing uses the
service's open (anonymous) mode by default; set `NETSU_RENDEZKEY_TOKEN` only if
your instance requires a token (privileged tier). Override the endpoint with
`--rendezkey-url`. One code can be claimed several times (`--rendezkey-reads`,
default 5) so it survives reconnects. Both ends print a per-interval speed log
and a summary, as iperf3 does. The JSON result gains a `connection` block with
the observed path (direct/relay) and RTT.

The service implementation now lives in this repository at
[`../apps/rendez-key`](../apps/rendez-key). Its anonymous create limiter (10 per
60 seconds per IP per Cloudflare location by default), code TTL (up to one hour
for the anonymous tier), and claim count (up to five for that tier) are three
independent controls. CI should start the local Worker or provide
`NETSU_RENDEZKEY_TOKEN`; it should not repeatedly exercise the public anonymous
endpoint.

**Server placement & firewalls.** Unlike plain iperf3 (which needs an inbound
port opened on the server's firewall), iroh hole-punches — so _either_ machine
can be the server with **no firewall configuration**, as long as you use the
default mode (omit `--direct-only`): both sides send outbound packets that open
their firewalls, falling back to a relay if hole-punching fails. `--direct-only`
skips this and requires the peer to reach the server's UDP endpoint directly, so
a server behind a strict inbound firewall (e.g. Windows Defender) is unreachable
that way — use it only on a permissive LAN, or when the server side accepts
inbound. (For a Windows server on `--direct-only`, allow the binary through:
`netsh advfirewall firewall add rule name="netsu" dir=in action=allow program="…\netsu.exe" enable=yes`.)

### Multiplexing + priority latency lab (`netsu mux`)

Many prioritized, rate-limited streams over one connection, measuring whether a
high-priority stream keeps low latency while others load the link:

```sh
netsu mux local  --scenario input-file --duration 10s        # in-process smoke
netsu mux listen --direct-only                               # one device
netsu mux run <code|ticket> --scenario mixed --priorities graded --json-out r.json
netsu mux matrix --duration 5s --output-dir out              # required-v1 case set
```

Scenarios: `input-only`, `clipboard-only`, `file-only`, `input-file`, `mixed`,
and **`custom`** (`--stream prio=30,hz=125,deadline=100ms` /
`--stream prio=0,rate=800mbps` / `--stream prio=0,saturating`). Priorities use
real QUIC stream priorities; a probe stream (one with a deadline) is measured
via per-message RTT (HDR histogram), everything else is throughput load.
`--json-out` writes a schema'd result ([`schema/mux-result-v1.json`](schema/mux-result-v1.json));
`--samples-out` writes per-message RTT NDJSON. Network conditions run under
Docker + `tc/netem` — see [`mux-docker/`](mux-docker/) and
[`scripts/mux-matrix.sh`](scripts/mux-matrix.sh).

### TUI (`netsu tui`)

An interactive launcher for **cross-device** testing without memorizing flags:
on one machine choose _Host a speed test_, pick a transport (TCP / UDP /
WebSocket / iroh / Native QUIC / WebRTC), and share what the hosting screen
shows. TCP, UDP, WebSocket, and Native QUIC use an editable `host:port`; iroh
and WebRTC use a short code. On the other machine choose _Join a speed test_,
select the matching transport, and enter that address or code. Both ends then
show a live throughput dashboard and a summary with an equivalent CLI command.

```sh
cargo run --features tui,quic,webrtc -- tui       # Native QUIC + direct WebRTC
cargo run --features tui,iroh,quic,webrtc,ws -- tui # every optional transport
cargo run --features tui -- tui                    # offline loopback lab only
```

Use `Tab`/`Shift-Tab` to move between fields. WebRTC defaults to
`https://rendez-key.xc.huakun.tech/v1/signal` and
`stun:stun.cloudflare.com:3478`; override those editable defaults with
`NETSU_SIGNAL_URL` and comma-separated `NETSU_STUN_URLS`. TURN is deliberately
unsupported: if ICE cannot establish a direct path, the run fails without
relaying benchmark traffic. Native QUIC uses an ephemeral self-signed benchmark
certificate and explicitly disables client verification in its generated CLI
hint; use the CLI flags when authenticated certificates are required.

A `tui`-only build keeps just the in-process loopback lab. Socket transports
default to a local address editable on the host screen; iroh shares a
self-describing ticket and needs no address. With
`--features input-demo`, the keyboard/mouse sharing session (below) is launched
straight from the menu — the TUI collects the role and code, then hands the bare
terminal to the session (global input capture can't share the TUI screen).

### Keyboard/mouse demo (`--features input-demo`)

A separate example (never in the default binary) for _perceived_ latency —
share input between two devices over iroh while pushing bulk load:

```sh
# controlled device (receives input):
cargo run --example kbm-demo --features input-demo -- controlled --inject-input
# controller device (sends its input):
cargo run --example kbm-demo --features input-demo -- controller <code|ticket> --bulk-streams 2
```

Injection is opt-in (`--inject-input`); the controller stops on `q` or
Escape+Ctrl+Alt; held keys are always released on stop/disconnect.

## Verifying

[`scripts/verify.sh`](scripts/verify.sh) runs fmt, clippy, and tests across the
feature matrix (including native QUIC) plus release + iroh/mux smokes.
