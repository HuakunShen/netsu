# netsu — iroh transport + multiplexing lab — Design

Date: 2026-07-21
Status: draft, pending user review

## 1. Background & Goals

`iroh-mux-bench` (`~/Dev/iroh-mux-bench`, pinned to `iroh = "=1.0.2"`) is a
standalone Rust benchmark for stream multiplexing, per-stream priority, and
latency-under-load over a single iroh/QUIC connection. It also contains a
kbm (keyboard/mouse) sharing demo. We are folding its capabilities into
`netsu` (`netsu-rs/`) and retiring the standalone repo.

**The core research question we want netsu to answer** (the reason for this
migration): on one connection carrying mixed traffic, *when the link is loaded
but not saturated, does a high-priority stream keep low latency?* And how does
that change across network conditions (bandwidth / delay / jitter / loss) and
priority configurations?

`iroh` is an **optional cargo feature**. A default `cargo build` produces
today's iperf3-compatible netsu with no iroh dependency; `cargo build
--features iroh` adds the iroh transport, the `netsu mux` lab, and rendez-key
ticket exchange.

### The key structural insight

`iroh-mux-bench` is really **three** subsystems, which migrate to three
different places in netsu:

| Old subsystem | What it is | Destination in netsu | Rides iperf3 protocol? |
|---|---|---|---|
| `src/speed/*` | a *reimplementation* of iperf3-shaped throughput over iroh | **deleted** — replaced by making iroh a real netsu transport | n/a (superseded) |
| top-level `src/{runner,receiver,workload,matrix,netem,metrics,samples,result,resources,output,protocol}.rs` | the multiplexing / priority / latency-under-load **lab** | `netsu mux …` subcommand (a sibling subsystem) | ❌ cannot — needs QUIC stream priorities + ACK-RTT |
| `src/demo/*` (+ `monio`) | kbm sharing / perceived-latency ("体感延迟") tool | `examples/kbm-demo.rs` behind `input-demo` feature | ❌ separate protocol |

The old repo had to reimplement a mini-iperf3 (`src/speed`) because it had no
real iperf3 core. netsu already has one. So we **do not port `src/speed`** —
instead we add `Transport::Iroh` under netsu's genuine iperf3 client/server,
which yields real throughput/latency measurement with real iperf3 control
semantics, and is strictly more netsu-aligned than the source ever was. The
lab cannot ride the iperf3 wire protocol (it needs QUIC per-stream priorities
and an application-layer ACK latency channel), so it stays a sibling that
merely *wears* netsu's idioms.

### Non-goals

- No iroh datagram / unreliable mode (V1 = reliable QUIC streams only, matching
  the source). UDP-style loss/jitter statistics remain a property of the
  existing `--udp` TCP/UDP transport, not iroh.
- No browser client.
- The `netsu mux` protocol is netsu-internal; it is **not** iperf3-interop and
  makes no such claim.
- No prebuilt iroh-enabled binaries on Releases yet (feature-gated
  `cargo install --features iroh` first).

## 2. Feature flags, dependencies, repo layout

### Features (in `netsu-rs/Cargo.toml`)

```toml
[features]
default = []
iroh = [
  "dep:iroh", "dep:iroh-tickets", "dep:uuid", "dep:bytes",
  "dep:hdrhistogram", "dep:rand_chacha", "dep:sysinfo", "dep:postcard",
  "dep:schemars", "dep:humantime", "dep:reqwest", "dep:chrono",
]
tui = ["dep:ratatui", "dep:crossterm"]   # independent of iroh
input-demo = ["iroh", "dep:monio"]       # implies iroh
```

- Single `iroh` feature covers the transport, the `mux` lab, and rendez-key
  (confirmed: single feature, not split).
- Independent `tui` feature adds `ratatui` + `crossterm`. The TUI's throughput
  screens work with the default transports; its iroh-throughput + `mux` screens
  are additionally `cfg(feature = "iroh")` (hidden/disabled with a note when
  built without it). So `--features tui` gives a TUI for tcp/udp/ws; `--features
  tui,iroh` unlocks iroh throughput + the mux lab in the TUI. (`iroh` stays a
  single unchanged feature.)
- `input-demo` implies `iroh` and additionally pulls `monio = "=0.1.1"`
  (features `tokio`, `recorder`).
- All new deps are `optional = true`. Existing default deps unchanged. Pin
  `ratatui`/`crossterm` to current (ratatui 0.30.x; `ratatui::run`/`init`/
  `restore`, `DefaultTerminal`, `crossterm::event::EventStream` for async).
- Pin `iroh = "=1.0.2"` and `iroh-tickets = "=1.0.0"` (same as source); verify
  latest-compatible at implementation time. `reqwest` uses `rustls-tls`,
  `default-features = false` (no OpenSSL); it reuses hyper already in iroh's
  tree.

### Layout

```
netsu-rs/
├── src/
│   ├── lib.rs                 # `#[cfg(feature="iroh")] pub mod mux; pub mod p2p;`
│   ├── transport/
│   │   ├── tcp.rs  udp.rs  ws.rs
│   │   └── iroh.rs            # NEW: BytePipe (control bi-stream) + DataChannel (data bi-streams)
│   ├── p2p/                   # NEW (feature iroh): shared iroh plumbing
│   │   ├── mod.rs
│   │   ├── endpoint.rs        # builder(presets::Minimal|N0), ALPNs, send_fairness, direct_only
│   │   ├── addr.rs            # EndpointTicket build/parse; --peer resolution
│   │   ├── rendezkey.rs       # HTTP client: store/claim short code
│   │   └── observe.rs         # observe_connection → direct/relay/rtt/stats
│   ├── mux/                   # NEW (feature iroh): the lab
│   │   ├── mod.rs  config.rs  workload.rs  runner.rs  receiver.rs
│   │   ├── protocol.rs        # StreamHello / Control / Data / Ack frames (VersionedFrame)
│   │   ├── metrics.rs  samples.rs  result.rs  resources.rs  output.rs
│   │   ├── matrix.rs          # required-v1 case set + aggregation
│   │   └── netem.rs           # profile validation (application is in Docker)
│   ├── demo/                  # NEW (feature input-demo): kbm
│   │   ├── mod.rs input.rs session.rs protocol.rs monio_backend.rs
│   │   └── transport/{mod.rs, iroh.rs, tcp.rs}
│   └── tui/                   # NEW (feature tui): ratatui launcher + live dashboard
│       ├── mod.rs            # `netsu tui` entry; event loop (EventStream + tick + live chan)
│       ├── app.rs           # App state machine: Home → Config → Running → Summary
│       ├── forms.rs         # per-mode config forms → typed options + equivalent CLI string
│       ├── dashboard.rs     # live widgets: gauges, per-stream table, latency panel, sparkline
│       └── live.rs          # LiveSnapshot type + subscription from runners
├── examples/
│   ├── kbm-demo.rs           # [[example]] required-features = ["input-demo"]
│   └── write_mux_schema.rs   # emits schema/mux-result-v1.json
├── mux-docker/               # Docker + tc/netem harness (ported from source docker/ + compose)
│   ├── Dockerfile docker-compose.yml entrypoint.sh netem-profiles.json
├── scripts/mux-matrix.sh mux-smoke.sh
├── schema/mux-result-v1.json mux-samples-v1.ndjson
└── PROTOCOL.md               # += "iroh transport binding" + "mux lab protocol" sections
```

`main.rs` grows a `#[cfg(feature = "iroh")]` `Mux(MuxArgs)` subcommand, an
`--iroh` path on `server`/`client`, and a `#[cfg(feature = "tui")]`
`Tui(TuiArgs)` subcommand. Without a feature the corresponding surface is absent
and the flag errors with a rebuild hint (mirrors the source's `input-demo`
handling).

### Error handling convention

The core protocol lib keeps `NetsuError` (thiserror). The `mux` lab and `p2p`
modules use `anyhow` internally (as the source does — they are benchmark/orchestration
code, not the interop-critical wire lib) and surface through the CLI the same
way `main.rs` already maps failures to stderr + non-zero exit. This keeps the
port faithful without forcing every lab function onto `NetsuError`.

## 3. Subsystem A — iroh as netsu's 4th transport (throughput)

Goal: run netsu's **existing** iperf3-compatible throughput/latency test over
one iroh connection, unchanged control protocol.

### Mechanics

- `Transport::Iroh` added to the enum in `client.rs` (today `{Tcp, Ws}` + a
  separate `udp: bool`). `run_client` gains an `Iroh` arm; `server.rs` gains an
  iroh listen/accept path.
- **Control channel** = one QUIC **bidirectional** stream implementing
  `BytePipe` (cookie, signed state bytes, `[u32-len][JSON]` framing — all
  reused verbatim). `BytePipe` is RPITIT/single-implementor today; the iroh
  control stream is a concrete type, so this fits without dyn.
- **Data streams** = N QUIC bidirectional streams, each implementing
  `DataChannel` (`#[async_trait]`, already `Box<dyn DataChannel>` in
  `streams/runner.rs`). Opaque chunk read/write; the transfer accounting above
  is unchanged.
- **One connection multiplexes control + all data streams.** In iperf3-over-TCP
  the per-stream 37-byte cookie correlates N separate connections to a session;
  over one iroh connection that correlation is intrinsic, so the cookie
  preamble is kept-but-redundant (documented no-op check) — no protocol change,
  just a transport binding note in `PROTOCOL.md`.
- Server accept: accept one iroh `Connection`, then `accept_bi()` the control
  stream first, then `accept_bi()` the N data streams. netsu's existing
  single-test lock and iperf3 accept rule still apply.
- `EXCHANGE_RESULTS` / sender+receiver byte reconciliation works identically
  (QUIC is reliable and ordered).

### CLI

```
netsu server --iroh [--direct-only] [--no-rendezkey] [--rendezkey-url URL]
# prints a rendez-key short code AND the full ticket, then accepts iroh clients

netsu client <ticket-or-code> --iroh \
    [-t 10] [-P 1] [-R] [-b 1M] [-l 128K] [-i 1] [--json] [--direct-only]
```

- With `--iroh`, the positional `host` argument is interpreted as a ticket or a
  rendez-key code (see §5). Port flag is ignored.
- No `--udp` over iroh (mutually exclusive; QUIC is reliable). `-R`, `-P`, `-b`
  (send pacing), `-l`, `-i`, `--json` all behave as today.
- `--direct-only`: build the endpoint with `presets::Minimal` (no relay/discovery)
  and **fail the run** if the selected path is a relay (carried from source's
  speed subsystem, where it is enforced).

### Result additions (iroh only)

The JSON result gains a `connection` block: `observedPath` (`"direct"` |
`"relay"`), `transportRttSamplesUs`, and iroh connection stats. Non-iroh runs
omit it. Existing iperf3-aligned fields are unchanged.

## 4. Shared iroh module (`src/p2p/`)

Used by **both** the throughput transport and the mux lab.

- **`endpoint.rs`**: `bind_listener`/`bind_client` over
  `Endpoint::builder(presets::Minimal)` (local / `--direct-only`, relay+discovery
  off) or `presets::N0` (routable). Applies
  `QuicTransportConfig::builder().send_fairness(bool)`. Distinct ALPNs so
  subsystems can't cross-connect: `netsu/iperf3-iroh/1` (throughput),
  `netsu/mux/1` (lab), `netsu/kbm-demo/1` (demo).
- **`observe.rs`**: `observe_connection(&Connection)` → `{ stable_id, paths:
  [{remote_addr, is_ip, is_relay, selected, rtt_us}], stats }`; maps the
  selected path to `direct|relay` and collects transport-RTT samples. Ported
  from source `transport.rs::observe_connection`.
- **`addr.rs`** + **`rendezkey.rs`**: addressing — see §5.

## 5. Addressing & rendez-key (the 8-char code)

**Requirement (confirmed):** default to sharing a short ~8-char rendez-key code
so it can be typed by hand; also print the full ticket; the connecting side
distinguishes the two by string length.

### rendez-key contract (from the `rendez-key` skill)

- Base URL `https://rendez-key.huakun.workers.dev` (override `--rendezkey-url`).
- **Store**: `POST /v1/entries?ttl=<s>&reads=<n>` with `Authorization: Bearer
  <token>`, `Content-Type: text/plain`, body = ticket → returns a code like
  `7K3M-Q9TX` (JSON `{code,expires_at,max_reads}` or bare with `Accept:
  text/plain`). `ttl` default 3600 (min 60, max 604800); `reads` default 1
  (max 100). Body ≤ 64 KiB (iroh tickets are far smaller).
- **Claim**: `POST /v1/entries/{code}/claim`, **no auth**, empty body →
  original ticket string. Code is case-insensitive; hyphen/space/none all
  accepted. `404 entry_not_available` uniformly for invalid/unknown/expired/
  exhausted.
- **Never store secrets** — plaintext at the worker/D1. Iroh tickets are
  short-lived connection addresses, which is exactly the intended use.

### Listener flow (`netsu … listen` / `server --iroh` / `mux listen`)

1. Build `EndpointTicket::new(endpoint.addr())`.
2. If a token is available (env `NETSU_RENDEZKEY_TOKEN`, fallback
   `RENDEZKEY_TOKEN`) and `--no-rendezkey` not set: store the ticket, get a
   code. `ttl` = `--rendezkey-ttl` (default 3600), `reads` = `--rendezkey-reads`
   (default 1).
3. Print **both**:
   ```
   code:   7K3M-Q9TX     (share this — expires in 60m, 1 claim)
   ticket: node1abc…      (or paste this directly)
   ```
   `--json` mode emits `{ endpointTicket, endpointId, endpointAddr, rendezkeyCode?, … }`.
4. No token / `--no-rendezkey` / store failure → print ticket only, with a hint
   (non-fatal; the ticket is always a valid fallback).

### Connecting flow (`--peer <arg>` / positional)

`resolve_peer(arg) -> EndpointTicket`:
- Normalize (strip spaces/hyphens). If the normalized length is **short**
  (≤ 16 chars — a rendez-key code is ~8, a ticket is hundreds), treat as a
  rendez-key **code** → `claim` (no token) → parse the returned string as an
  `EndpointTicket`.
- Otherwise parse `arg` directly as an `EndpointTicket`.
- The 16-char threshold is comfortably between the two populations; a real
  ticket never falls under it and a code never exceeds it.

## 6. Subsystem B — the `netsu mux` lab

The heart of the migration. Ported faithfully, renamed to netsu idioms.

### Subcommands

```
netsu mux listen  [--json] [--direct-only] [rendez-key opts]
netsu mux run     --peer <ticket|code> [--topology auto|container]
                  [--direct-only] [--connect-timeout 30s] <RunArgs…>
netsu mux local   <RunArgs…>          # two in-process endpoints; smoke/CI
netsu mux matrix  [--peer …] [--profile required-v1] [--repetitions 3]
                  [--seed 12345] [--output-dir DIR] [--direct-only]
```

### 6.1 Workload model — 4 presets **+ custom**

A **workload kind** = a *type of traffic* (not load magnitude). Each kind is a
paced generator of deterministic bytes (seeded ChaCha8, reproducible) on its
own stream(s), with a priority, a rate, and — for latency-sensitive kinds — a
deadline.

| Kind | Models | Defaults | Configurable |
|---|---|---|---|
| `Input` | keyboard/mouse | 64 B, 125 Hz, deadline 100 ms | payload, hz, deadline, priority |
| `Clipboard` | clipboard bursts | sizes 1K/16K/64K, interval 250 ms–1 s, deadline 1 s | sizes, interval range, deadline, priority |
| `Cast` | screen share | 20 Mbps, 16 KiB chunks, 1 stream, 5 ms pacing | bitrate, chunk, streams, pacing, priority |
| `File` | bulk transfer | saturating, 64 KiB chunks, 1 stream | mode (saturating\|fixed-rate), rate, chunk, streams, priority |
| `Control`/`Ack` | internal (handshake + latency channel) | priority 40 | — |

Load/pressure comes from `File` (saturating) and `Cast` bitrate; `Input` is the
measured probe. Named **scenarios** select which kinds are active:
`input-only`, `clipboard-only`, `file-only`, `input-file`, `mixed`.

**`custom` scenario (new).** A general per-stream form. Repeatable `--stream`
flag; each defines one stream (or `count=N` identical streams):

```
--scenario custom \
  --stream role=probe,prio=30,hz=125,payload=64,deadline=100ms \
  --stream role=load,prio=0,rate=800mbps,chunk=65536,count=2 \
  --stream role=load,prio=0,saturating
```

`--stream` grammar (comma-separated `key=value`):

| key | meaning | default |
|---|---|---|
| `prio` | i32 QUIC priority (required) | — |
| `rate` | `<n>mbps` fixed-rate pacing | — |
| `saturating` | greedy (mutually exclusive with `rate`) | — |
| `hz` | pacing frequency (small-probe alternative to `rate`) | — |
| `payload` | bytes per item | 64 (probe) / 65536 (load) |
| `chunk` | max write size | = payload |
| `deadline` | duration; **presence ⇒ measured probe** (ACK-RTT + deadline miss) | none ⇒ load |
| `role` | `probe`\|`load` (explicit; else inferred from `deadline`) | inferred |
| `count` | number of identical streams | 1 |

Internally this generalizes the source's
`workload_specs() -> Vec<(kind, index, priority)>` and per-kind generators into
a `Vec<StreamSpec>` carrying `{priority, pacing (rate|saturating|hz), payload
pattern, measured?}`. The 4 presets become named `StreamSpec` builders; the
producer/writer/ACK machinery (§6.3) is shared. "Which streams are measured"
moves from `kind ∈ {Input,Clipboard}` to a per-stream `measured` flag.

### 6.2 Priority (static + dynamic)

- Real QUIC priority via `SendStream::set_priority(i32)` at stream open
  (`p2p`/mux `open_prioritized_uni`/`_bi`), higher = scheduled first. Data
  streams are **unidirectional** per workload; control is bidirectional; the
  receiver opens one uni **Ack** stream back to the sender.
- Presets `PriorityConfig::{equal, graded(ack40/input30/clip20/cast10/file0),
  inverted}`; each kind overridable (`--input-priority`, …). Custom streams
  carry their own `prio`.
- `send_fairness` (`--send-fairness true|false`) is a transport knob affecting
  only equal-priority streams.
- **Dynamic change**: `--priority-change-after <dur> --priority-change-workload
  <kind> --priority-change-to <i32>` → mid-run `set_priority`, recorded as a
  `PriorityChangeObservation{old,new,requested_after_ms,applied_elapsed_ms,
  bytes_before,bytes_after}` with a warning that the timestamp is API-call time,
  not effective-scheduling time.

### 6.3 Measurement (application-layer ACK-RTT)

- Sender registers `(stream, sequence) → Instant` in an `AckTracker` (bounded)
  for **measured** streams; item is counted only if scheduled after the warmup
  boundary (`measurement_start`, propagated to the receiver via a `measured`
  bit on the data header so warmup/cooldown bytes are excluded).
- Receiver echoes `Ack{stream, sequence, status}` on the Ack stream after the
  full logical message arrives. Sender computes RTT → `LatencyRecorder`
  (`hdrhistogram`, 1 µs…3.6 s, 3 sig figs) + Welford mean/stddev + successive-
  diff jitter + deadline-exceeded counting. Beyond `--ack-timeout` counts as a
  deadline miss; stragglers swept at end.
- Rationale (documented): monotonic `Instant`, no wall-clock sync between hosts;
  this is an application round-trip, deliberately not a QUIC packet RTT.

### 6.4 Rate limiting

Application-level scheduled pacing via a `Pacer` (`sleep_until(next); next +=
interval`), **not** a token bucket, **not** QUIC pacing. `Cast` bytes/tick from
bitrate; `File` fixed-rate interval from `chunk*8/rate`; `File` saturating =
`yield_now` fill loop; per-kind rate split across that kind's N streams.
Producer→writer backpressure is a bounded `mpsc(8)`.

### 6.5 Matrix (`mux matrix --profile required-v1`)

The 14 groups → 20 cases (asserted), enumerated × `--repetitions` (seed =
base + rep): `input-unloaded`, `clipboard-unloaded`, `file-saturating`,
`input-file-equal`, `mixed-{equal,graded,inverted}`, `equal-fairness-{on,off}`,
`multi-stream-fairness` (4 Cast + 4 File), `chunk-size-sweep{,-16k,-64k,-256k}`,
`concurrent-stream-sweep{,-2,-4,-8}`, `starvation-progress` (does a low-prio
File still progress under 4×Cast prio 40?), `dynamic-priority-change`. Writes
`<case>-<NN>.json` + `.ndjson` and a `comparison.json` =
`ComparisonReport{profile, runs, aggregates,
load_induced_input_p99_delta_us}` — the headline metric (loaded input p99 −
unloaded input p99, only over compatible runs). Aggregation = per-case means of
input/clipboard p99, cast bitrate, file throughput, Jain fairness, and
`all_file_streams_progressed`.

### 6.6 Network conditions (`mux-docker/`)

`netem.rs` **validates only** (`NetemProfile{rate,delay,jitter,loss,reorder,
limit}`, regex-guarded against shell metacharacters — an injection guard).
Application is Linux `tc`/`netem` in Docker (`NET_ADMIN`): `entrypoint.sh`
re-validates each `NETEM_*` env var (exit 64 on bad input) then
`tc qdisc replace dev eth0 root netem rate … delay … <jitter> loss … reorder …
limit …`. Named profiles (`netem-profiles.json`): `baseline` (500mbit/10ms),
`constrained` (100mbit/50ms/5ms/0.1%), `slow` (20mbit/200ms/20ms/1%),
`long-haul` (100mbit/500ms), `lossy` (100mbit/5%/0.1% reorder). Driven by
`scripts/mux-matrix.sh` (one full matrix per profile via `jq`, isolated
`COMPOSE_PROJECT_NAME`, trap cleanup). On macOS this runs inside Docker's Linux
VM — the result carries a `Container` topology warning (it validates controlled
queueing, not native macOS networking).

### 6.7 Protocol (`src/mux/protocol.rs`)

Reuse netsu's framing idiom via a `VersionedFrame`-style trait with a per-frame
`EXPECTED_VERSION`; `postcard` (alloc) body, length-prefixed, size-capped,
rejecting oversized frames before allocation and unknown versions. Frames:
`StreamHello{version, run_id, workload/stream-kind, stream_index}` (first write
on every stream), `Control::{Start, Ready, Finish, Finished}` (bi control),
`DataHeader{sequence, measured, len}` + payload, `Ack{workload, sequence,
status}`.

## 7. Output & schemas

- **`mux/result.rs`**: `MuxResult` with `schemaVersion=1`, `benchmarkVersion`,
  `irohVersion`, `gitCommit` (via `build.rs`, ported), `startedAt`, `seed`,
  `topology` (`Local|Container|Lan|Relay`), `scenario`, `config`, `priorities`,
  `transport`, per-workload summaries (`Input`/`Clipboard` latency; `Cast`
  requested/effective/receiver bitrate + pacing misses + jitter; `File`
  aggregate + per-stream throughput + Jain fairness + progress), `resources`
  (CPU%/RSS mean+max via `sysinfo`), `connection` (paths, stats, priority
  changes, bytes, close reason), `warnings`, `replayCommand`. serde
  `camelCase`; derives `schemars::JsonSchema` → `schema/mux-result-v1.json`
  (emitted by `examples/write_mux_schema.rs`). `validate_schema()` cross-checks
  config vs summary and reconciles sent-vs-received bytes per stream.
- **`mux/samples.rs`**: NDJSON diagnostic stream. `Sample{schema_version,
  run_id, elapsed_us, workload, metric, sequence, stream_index, value}`,
  `metric ∈ {RttUs, QueueDelayUs, WriteBlockedUs, EventLoopLatenessUs,
  InterArrivalUs, TransportRttUs, Bytes, PriorityChange}`. Async writer with a
  bounded `mpsc(8192)` that **drops (counted) when full** so the hot path never
  blocks on disk; aggregate metrics stay exact; dropped count is a warning.
  Atomic write (temp + `sync_all` + rename).
- **`mux/output.rs`**: `write_json_atomic` (pretty, temp+fsync+rename) +
  `human_summary` emphasizing tail latency and measurement scope.

## 8. Subsystem D — kbm demo (`examples/kbm-demo.rs`, feature `input-demo`)

- The demo is **not** a `netsu` subcommand; it is a standalone example, so it
  never ships in the default binary and is run explicitly.
- `src/demo/` (feature-gated) holds the ported transport-free core
  (`NormalizedInputEvent`, `InputQueue` coalescing, `InputGate` replay
  rejection, `PressedState`, `InputInjector`), the transport-agnostic
  `run_controller`/`run_controlled` sessions + `DemoFrame` protocol, the two
  transports (`iroh` with priorities ack/safety 40, input 30, bulk 0; `tcp`
  `TCP_NODELAY` LAN baseline), and the single `monio`-dependent
  `monio_backend.rs` (global capture/injection + emergency chord). `examples/
  kbm-demo.rs` is a thin clap entrypoint exposing `listen`/`control` roles.
- Safety carried verbatim: `--inject-input` required opt-in, `--allow-peer`
  pins an `EndpointId`, `q` / `Ctrl+Alt+Esc` / idle-timeout → release-all. Bulk
  load reuses the lab's `Pacer`/`DeterministicBytes` so you can *feel* latency
  under load (`--bulk-streams`, `--bulk-rate-mbps`).

## 9. Subsystem E — TUI (`netsu tui`, feature `tui`)

Net-new (not migrated from the source). A ratatui launcher + live dashboard so
users pick modes/presets and watch results instead of memorizing flags.
Confirmed scope: **launcher + live dashboard**, driving **both** the mux lab and
the iperf3 throughput test. Runs the selected test **in-process** (same binary,
same types, a live channel) — it does not shell out.

### Event loop

`netsu tui` → `ratatui::init()` → an async loop over `tokio::select!` of three
sources, redrawing on any: `crossterm::event::EventStream` (keys/resize), a
~10 Hz tick (`tokio::time::interval`), and a `mpsc::Receiver<LiveSnapshot>`
(live metrics from the running test). `ratatui::restore()` on exit (and on
panic via a hook) so the terminal is never left raw.

### App states

1. **Home** — pick a top-level mode: Throughput *client* / Throughput *server*
   / Mux *run* / Mux *listen* / Mux *matrix* / Mux *local*. Iroh/mux entries are
   shown-but-disabled with a hint when built without `--features iroh`.
2. **Config form** (`forms.rs`) — fields per mode, all point-and-select:
   - *Throughput*: transport (tcp/udp/ws/iroh), host **or** peer code/ticket,
     duration, parallel, reverse, bandwidth, len, direct-only.
   - *Mux run*: scenario (input-only/…/mixed/**custom**), priority preset
     (equal/graded/inverted) or per-kind override, key params (duration, file
     rate/mode, cast bitrate, streams), transport opts (direct-only), peer
     input, optional dynamic priority-change. **custom** opens a small stream
     editor: add/remove rows of `prio / rate|saturating / deadline / count`
     (the §6.1 grammar, built visually).
   - *Mux matrix*: profile, repetitions, seed, output dir, peer.
   Each form renders a live **"equivalent CLI command"** line (teaches the flags
   and is copyable), and validates before Run.
3. **Running / live dashboard** (`dashboard.rs`) — driven by `LiveSnapshot`:
   - Throughput: a `Gauge`/`Sparkline` of current Mbps (sender & receiver),
     elapsed/remaining, connection type (direct/relay) + transport RTT for iroh.
   - Mux: a per-stream `Table` (kind/index, priority, target rate, live Mbps,
     bytes); a **latency panel** for probe streams (live p50/p99, deadline-miss
     count) with an RTT `Sparkline`/`Chart`; aggregate throughput + Jain
     fairness; CPU/RSS. This is where you *see* input p99 stay flat (or spike)
     as file throughput climbs. `Esc`/`q` aborts cleanly (cancels the run task).
   - Listener/server modes prominently show the rendez-key **code + ticket** and
     a "waiting → connected (direct/relay)" status before the dashboard.
4. **Summary** — final numbers, the path to any written JSON/NDJSON, and the
   equivalent CLI command for reproducing headlessly. `r` re-runs, `Esc` returns
   Home.

### Live plumbing

Both runners gain an optional `LiveObserver` (an `Option<mpsc::Sender<
LiveSnapshot>>`) emitted every ~100–250 ms; headless CLI runs pass `None` and
are unaffected. Throughput reuses the existing `on_interval` callback to feed
the same channel; the mux runner already tracks per-stream counters +
`LatencyRecorder`, so it adds a periodic snapshot emit alongside its sample
writer. `LiveSnapshot` is a small owned struct (per-stream Mbps/bytes, probe
p50/p99 rolling, elapsed, connection info) — the TUI never touches runner
internals.

## 10. Testing strategy

Port the source's ~26-file suite, adapted to netsu, plus new coverage:

- **Multiplexing**: all lab streams share **one** iroh connection (single
  `stable_id`, ≥ N stream ids); `input-only` opens no background streams.
- **Priority**: graded order 40/30/20/10/0; `dynamic_priority_change_is_recorded_with_progress`.
- **Rate/pacing**: `cast_budget_matches_target_bitrate`,
  `fixed_rate_file_interval_matches_chunk_budget`, `Pacer` lateness without
  drift (`start_paused`); **custom-stream** parsing + probe/load classification.
- **Netem**: named profiles cover required values; values reject shell
  metacharacters/invalid units; `entrypoint.sh` rejects injected rate (exit 64).
- **Metrics/results/output/samples/protocol**: latency tail/dispersion; Jain
  fairness edges; ACK match-once + timeout + boundedness; schema-v1 fields +
  checked-in schema; sample is one JSON line; hot-path emitter drops instead of
  blocking; atomic JSON replace; frame round-trips; oversized/unknown-version
  rejection; truncated-vs-clean-EOF.
- **Failure paths**: receiver rejects non-control first stream; reports reset
  stream; sender returns when receiver closes early.
- **NEW — iroh throughput transport**: upload/reverse/parallel over one iroh
  connection reconciles byte-exact; `--direct-only` fails on a relay path;
  interop with netsu's real iperf3 control state machine.
- **NEW — rendez-key**: `resolve_peer` length discrimination (code vs ticket);
  claim happy path; no-token / `--no-rendezkey` fallback prints ticket;
  uniform-404 handling.
- **CLI process**: `mux local` writes JSON+NDJSON; `mux listen --json` emits a
  parseable ticket/code; default build (no `iroh`) still builds and `--iroh`
  errors cleanly.
- **NEW — TUI** (feature `tui`): form-state → typed options mapping (incl. the
  custom stream editor → `StreamSpec`s) and the "equivalent CLI command" string
  are pure and unit-tested; dashboard rendering from a synthetic `LiveSnapshot`
  is snapshot-tested with ratatui `TestBackend` (`assert_buffer`); iroh/mux
  screens are absent and Home hints them when built without `--features iroh`.

`scripts/mux-smoke.sh` + a netsu-wide `verify.sh` extension gate fmt / clippy
`-D warnings` / `test --features iroh,tui` / a release `mux local` smoke / a
real `mux listen`+`run` direct smoke / a headless TUI build check.

## 11. Migration & retirement of `iroh-mux-bench`

Everything worth keeping moves: `speed/*` → the iroh transport (concept, not
code); the lab modules → `src/mux/`; `demo/*` → `src/demo/` + example;
`docker/` + `docker-compose.yml` → `mux-docker/`; `scripts/` → `scripts/mux-*`;
`schema/` + `examples/*.json|ndjson` → `schema/`; the two design specs + the two
empirical write-ups (`docs/latency-under-load-*.md`,
`docs/speed-comparison-*.md`) → `docs/` for reference. After the migration lands
and `verify.sh` passes, `iroh-mux-bench` is retired (recoverable from its own
git history). Nothing unique is left behind.

## 12. Risks & open items

- **iroh version pinning**: confirm `=1.0.2` / `iroh-tickets =1.0.0` still
  resolve with netsu's tree at implementation time; bump together if needed.
- **Dependency weight**: the `iroh` feature is heavy (iroh + sysinfo +
  hdrhistogram + schemars + reqwest). Acceptable because it is fully optional
  and off by default. Could later split `iroh` (transport) vs `iroh-lab` if the
  transport alone is wanted lean — deferred (confirmed: single feature now).
- **`BytePipe` single-implementor assumption**: iroh's control stream becomes a
  second concrete implementor; confirm the RPITIT dispatch in `client.rs` still
  monomorphizes cleanly per transport (it should — dispatch is by transport
  arm, not dyn).
- **rendez-key availability**: the worker is a third-party dependency of the
  *convenience* path only; the ticket is always a working fallback, and
  `--no-rendezkey` fully bypasses it. No secret is ever uploaded.
- **macOS netem**: conditions apply only inside Docker's Linux VM; native macOS
  path shaping is out of scope (documented limitation, inherited from source).
- **TUI terminal restore**: raw mode must be restored on normal exit *and*
  panic; install a panic hook that calls `ratatui::restore()` so a mid-run crash
  never leaves the terminal wedged. TUI logic (form→options, snapshot render) is
  kept pure/testable; only the thin event loop is not.

## 13. Implementation phasing (for the plan)

1. `iroh` feature scaffold + `p2p` (endpoint, observe) + `Transport::Iroh`
   throughput (client/server), no rendez-key yet — get iroh throughput green.
2. rendez-key (`p2p::addr`/`rendezkey`) + `--peer` resolution + listener dual
   print, wired into both throughput and (later) mux.
3. `mux` lab core: protocol, config (4 presets + custom), workload/pacer,
   runner/receiver, metrics, ACK-RTT, `mux local` + `mux run`/`listen`.
4. `mux` output/result/samples/resources + schemas.
5. `mux matrix` + `mux-docker/` + netem + scripts.
6. `tui` feature: `LiveObserver`/`LiveSnapshot` plumbing on both runners, then
   the launcher (Home + config forms + equivalent-CLI) and the live dashboard;
   `TestBackend` snapshot tests. (Depends on 1–4; matrix screen after 5.)
7. kbm demo (`input-demo`) + example.
8. Retire `iroh-mux-bench`; docs; `verify.sh` extension.
