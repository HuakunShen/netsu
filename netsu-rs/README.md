# netsu (Rust)

An iperf3-compatible network speed test — a library and a CLI.

netsu speaks **iperf3's wire protocol**, so it interoperates with the official
`iperf3` binary in both directions (netsu client ↔ iperf3 server, iperf3 client
↔ netsu server) over TCP and UDP. It also has a WebSocket transport as a
netsu-only extension (HTTP-proxy traversable; official iperf3 can't speak it).

This is the Rust implementation. A matching TypeScript implementation lives in
[`packages/netsu`](../packages/netsu); the two are protocol-compatible and
share the wire-protocol spec in [`../PROTOCOL.md`](../PROTOCOL.md).

## Install

```sh
cargo install netsu
```

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
