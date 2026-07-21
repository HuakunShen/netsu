# netsu (Rust)

An iperf3-compatible network speed test — a library and a CLI.

netsu speaks **iperf3's wire protocol**, so it interoperates with the official
`iperf3` binary in both directions (netsu client ↔ iperf3 server, iperf3 client
↔ netsu server) over TCP and UDP.

Beyond the iperf3 core, netsu has optional, opt-in capabilities behind cargo
features (see [Optional features](#optional-features)): a WebSocket transport,
an **iroh/QUIC transport** with short shareable codes, a **multiplexing +
priority latency lab** (`netsu mux`), an interactive **cross-device TUI**
(host/join two machines by sharing a code), and a keyboard/mouse sharing **demo**.

This is the Rust implementation. A matching TypeScript implementation lives in
[`packages/netsu`](../packages/netsu); the two are protocol-compatible and
share the wire-protocol spec in [`../PROTOCOL.md`](../PROTOCOL.md).

## Install

```sh
cargo install netsu                          # lean TCP/UDP core (~1 MB binary)
cargo install netsu --features iroh,tui      # + iroh transport, mux lab, TUI
```

The default build is the smallest possible iperf3-compatible TCP+UDP tool
(~980 KB, stripped, LTO). Every non-core transport/UI is opt-in so it only adds
binary size when you enable it — see [Optional features](#optional-features).
(A Rust tokio/clap binary can't reach iperf3's 192 KB C footprint; `std + tokio
+ clap` is a ~1 MB floor.)

## CLI

```
netsu server [-p 5201] [--ws]
netsu client <host> [-p 5201] [-u | --ws] [-t 10] [-P 1] [-R] [-b 1M] [-l 128K] [-i 1] [--json]
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

`ClientOptions` covers transport (`Tcp`/`Ws`), `udp`, `reverse`, `duration`,
`parallel`, `len`, `bandwidth`, and an `interval` callback; `TestResult` carries
both sides' byte counts, per-stream results, and (for UDP) jitter/loss.

Run a server from the library with `netsu::server::start_server(ServerOptions {
.. })`, which returns a handle whose `close()` releases the port.

## Protocol

The wire protocol is documented in [`../PROTOCOL.md`](../PROTOCOL.md) and is the
single source of truth shared by both implementations. Conformance is checked
against the real `iperf3` binary in this crate's integration tests.

## Optional features

| Feature | Adds | Extra deps |
|---|---|---|
| `ws` | WebSocket transport (`--ws`), HTTP-proxy traversable | tokio-tungstenite |
| `iroh` | iroh/QUIC transport (`--iroh`) + `netsu mux` lab + rendez-key codes | iroh, reqwest, hdrhistogram, … |
| `tui` | `netsu tui` — cross-device host/join launcher + live dashboard | ratatui |
| `input-demo` | `examples/kbm-demo` keyboard/mouse sharing (implies `iroh`) | monio |

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

**Server placement & firewalls.** Unlike plain iperf3 (which needs an inbound
port opened on the server's firewall), iroh hole-punches — so *either* machine
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
on one machine choose *Host a speed test*, pick a transport (TCP / UDP /
WebSocket / iroh), and the TUI shows a short **code**; on the other machine
choose *Join a speed test* and type that code — both ends then show a live
throughput dashboard and a summary. One code works for every transport (it
carries a `tag|addr` blob, so the joiner needs no separate transport pick and no
long ticket), and it stays valid for several joins.

```sh
cargo run --features tui,iroh -- tui        # full cross-device experience
cargo run --features tui -- tui             # slim build: offline loopback lab only
```

The code-based flow needs `--features iroh` (rendez-key rides `reqwest`); a
`tui`-only build keeps just the in-process loopback "lab" runs (throughput +,
with iroh, the mux scenarios). Socket transports advertise this host's detected
LAN IP, editable on the host screen (a VPN/tunnel default route can mis-detect
it); iroh shares a self-describing ticket and needs no address. With
`--features input-demo`, the keyboard/mouse sharing session (below) is launched
straight from the menu — the TUI collects the role and code, then hands the bare
terminal to the session (global input capture can't share the TUI screen).

### Keyboard/mouse demo (`--features input-demo`)

A separate example (never in the default binary) for *perceived* latency —
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
feature matrix plus release + iroh/mux smokes.
