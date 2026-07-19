# netsu TS Rewrite (Phase 1 of 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the TypeScript implementation of netsu speaking iperf3's wire protocol, verified against official iperf3 on localhost.

**Architecture:** Control state machine (iperf3 protocol, byte-exact) over a transport-agnostic "byte pipe"; data plane in separate connections; pure-logic `protocol/` and `stats` modules with no socket access. See `docs/superpowers/specs/2026-07-20-netsu-rewrite-design.md`.

**Tech Stack:** TypeScript (ESM only), bun, tsdown (create-tsdown default template), vitest, valibot, `ws`, citty. Official iperf3 binary as test referee.

**Phase context:** This is Plan 1 of 3. Plan 2 (Rust implementation) and Plan 3 (docker e2e interop matrix + CI overhaul) are written after this phase completes, informed by the actual PROTOCOL.md and code produced here.

## Global Constraints

- Package: npm name `netsu`, JSR `@hk/netsu`, version `0.2.0`, ESM-only, `engines.node >= 20`.
- All source under `packages/netsu/src/`; tests under `packages/netsu/tests/`.
- `protocol/` and `stats.ts` must never import `node:net`, `node:dgram`, or `ws`.
- Strict TS. No `any`. `bun run typecheck` (tsc --noEmit) must pass at every commit.
- Tests use ports 5210–5260 (never 5201) to avoid clashing with a real iperf3.
- Integration tests against official iperf3 are wrapped in `describe.skipIf(!HAS_IPERF3)`; `HAS_IPERF3` detects the binary at runtime.
- Protocol constants and JSON field names come from iperf3 source (values are embedded in Task 2's PROTOCOL.md — treat that file as the authority for later tasks).
- Commit after every task (conventional commits). Run commands from `packages/netsu/` unless stated otherwise.
- iperf3 servers spawned by tests use `-1` (one-off mode) where noted, and every test uses a unique port.

---

### Task 1: Demolition and fresh scaffold

Delete the dead implementations, scaffold the new package with create-tsdown, wire it into the bun workspace.

**Files:**
- Delete: `go/` (entire dir), `packages/netsu/` (entire dir), `netsu-rs/target/` (untracked build junk)
- Create: `packages/netsu/` (fresh scaffold), `packages/netsu/.npmrc` not needed
- Modify: root `package.json` (no change needed — workspace glob already covers it)

**Interfaces:**
- Consumes: nothing
- Produces: a building, testing, typechecking empty package named `netsu@0.2.0`

- [ ] **Step 1: Verify toolchain**

```bash
bun --version                 # any 1.x
iperf3 --version || brew install iperf3
```
Expected: iperf3 version 3.x printed.

- [ ] **Step 2: Delete dead code**

```bash
cd /Users/hk/Dev/netsu
git rm -r go packages/netsu
rm -rf netsu-rs/target
git commit -m "chore: remove go impl and broken ts impl ahead of rewrite"
```

- [ ] **Step 3: Scaffold new package**

```bash
cd /Users/hk/Dev/netsu/packages
bunx create-tsdown@latest netsu -t default
```

- [ ] **Step 4: Adapt scaffold to this repo**

Edit `packages/netsu/package.json` — set these fields (keep the scaffold's other fields and devDependencies):

```json
{
  "name": "netsu",
  "version": "0.2.0",
  "description": "iperf3-compatible network speed test — library and CLI",
  "license": "MIT",
  "type": "module",
  "engines": { "node": ">=20" },
  "bin": { "netsu": "./dist/cli.mjs" },
  "scripts": {
    "build": "tsdown",
    "dev": "tsdown --watch",
    "test": "vitest --run",
    "typecheck": "tsc --noEmit",
    "prepublishOnly": "bun run build"
  },
  "dependencies": {
    "citty": "^0.1.6",
    "valibot": "^1.0.0",
    "ws": "^8.18.0"
  }
}
```

Edit `packages/netsu/tsdown.config.ts`:

```ts
import { defineConfig } from "tsdown";

export default defineConfig({
  entry: ["src/index.ts", "src/cli.ts"],
  dts: { tsgo: true },
  exports: true,
  platform: "node",
});
```

Add dev dependency for ws types and install:

```bash
cd /Users/hk/Dev/netsu/packages/netsu
bun add -d @types/ws
cd /Users/hk/Dev/netsu && bun install
```

Replace `packages/netsu/src/index.ts` with a placeholder that Task 12 finalizes:

```ts
export const VERSION = "0.2.0";
```

Create `packages/netsu/src/cli.ts`:

```ts
#!/usr/bin/env node
console.log("netsu cli placeholder");
```

Delete the scaffold's example test; create `packages/netsu/tests/smoke.test.ts`:

```ts
import { expect, it } from "vitest";
import { VERSION } from "../src/index.ts";

it("exports version", () => {
  expect(VERSION).toBe("0.2.0");
});
```

If the scaffold's `tsconfig.json` lacks `"allowImportingTsExtensions": true`, add it (tests import `.ts` paths).

- [ ] **Step 5: Verify build, test, typecheck**

```bash
cd /Users/hk/Dev/netsu/packages/netsu
bun run typecheck && bun run test && bun run build
```
Expected: all pass; `dist/index.mjs`, `dist/cli.mjs` produced.

- [ ] **Step 6: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: scaffold fresh netsu ts package with tsdown"
```

---

### Task 2: PROTOCOL.md — the wire protocol authority

Every later task cites this file instead of re-deriving protocol facts. Content below is extracted from iperf3 source (`iperf_api.h`, `iperf_api.c`, `iperf_util.c`, `iperf_udp.c`, `iperf_tcp.c`, `iperf_server_api.c`, `iperf_client_api.c`).

**Files:**
- Create: `PROTOCOL.md` (repo root)

**Interfaces:**
- Consumes: nothing
- Produces: protocol constants and message schemas cited by Tasks 3–11

- [ ] **Step 1: Write PROTOCOL.md**

Write `/Users/hk/Dev/netsu/PROTOCOL.md` with exactly this content:

````markdown
# netsu Wire Protocol

netsu speaks the iperf3 wire protocol (verified against esnet/iperf master,
2026). Sections marked **[netsu extension]** apply only between netsu peers.

## Transport roles

- **Control channel**: one TCP connection (iperf3 mode) or one WebSocket
  connection [netsu extension]. Carries cookie, state bytes, JSON messages.
- **Data streams**: N separate TCP connections / UDP sockets / WebSocket
  connections [netsu extension]. Carry only test payload (plus a one-time
  cookie preamble on TCP/WS, or a connect handshake on UDP).

## Cookie

37 bytes: 36 random chars from alphabet `abcdefghijklmnopqrstuvwxyz234567`
followed by one NUL (`\0`). Generated by the client per test session.

- Control connection: client sends the 37 bytes immediately after connecting.
- Each TCP/WS data stream: client sends the same 37 bytes as its first write;
  the server compares (`strncmp` semantics: byte-equal over 37 bytes) against
  the active session's cookie and drops the connection on mismatch.

## State bytes

Single **signed** byte written on the control channel.

| State | Value | Sender |
|---|---|---|
| TEST_START | 1 | server |
| TEST_RUNNING | 2 | server |
| TEST_END | 4 | client |
| PARAM_EXCHANGE | 9 | server |
| CREATE_STREAMS | 10 | server |
| SERVER_TERMINATE | 11 | server |
| CLIENT_TERMINATE | 12 | client |
| EXCHANGE_RESULTS | 13 | server |
| DISPLAY_RESULTS | 14 | server |
| IPERF_START | 15 | server |
| IPERF_DONE | 16 | client |
| ACCESS_DENIED | -1 | server |
| SERVER_ERROR | -2 | server |

## JSON framing

`[4-byte unsigned big-endian length][UTF-8 JSON bytes]`. iperf3 caps
PARAM_EXCHANGE reads at 8 KiB (`MAX_PARAMS_JSON_STRING`); netsu caps all
JSON reads at 64 KiB.

## Test lifecycle

```
client                                  server
  │ ── connect control channel ───────► │
  │ ── cookie (37B) ──────────────────► │  busy? → state ACCESS_DENIED, close
  │ ◄── state PARAM_EXCHANGE ────────── │
  │ ── json params ───────────────────► │
  │ ◄── state CREATE_STREAMS ────────── │
  │ ══ open N data streams (cookie / UDP handshake each) ══► │
  │ ◄── state TEST_START ────────────── │  (after all N accepted)
  │ ◄── state TEST_RUNNING ──────────── │
  │ ══ payload flows (client→server; reversed when reverse=true) ══ │
  │ ── state TEST_END ────────────────► │  (client's duration timer fires)
  │ ◄── state EXCHANGE_RESULTS ──────── │
  │ ── json results (client's view) ──► │  client sends FIRST
  │ ◄── json results (server's view) ── │  then server sends
  │ ◄── state DISPLAY_RESULTS ───────── │
  │ ── state IPERF_DONE ──────────────► │
  │    both close everything            │
```

Connection acceptance rule (iperf3 `iperf_accept`): when a connection
arrives on the listening port, read 37 bytes. If no test is active, it is a
new control connection. If a test is active and the 37 bytes equal the
session cookie during CREATE_STREAMS, it is a data stream. Otherwise reply
ACCESS_DENIED and close.

## PARAM_EXCHANGE JSON (client → server)

netsu sends (subset of iperf3's; all fields iperf3-standard):

```json
{
  "tcp": true,            // OR "udp": true — exactly one present
  "omit": 0,
  "time": 10,             // seconds
  "num": 0,               // -n bytes mode: 0 = unused
  "blockcount": 0,        // -k blocks mode: 0 = unused
  "parallel": 1,
  "reverse": true,        // only present when reverse
  "len": 131072,          // blksize; UDP default 1460, TCP default 131072
  "bandwidth": 1048576,   // bits/s; only present for UDP (pacing)
  "pacing_timer": 1000,
  "client_version": "netsu-0.2.0"
}
```

Server must tolerate unknown fields (iperf3 sends many more). netsu-as-server
reads: `tcp`/`udp`, `time`, `parallel`, `reverse`, `len`, `bandwidth`; ignores
the rest; rejects `parallel > 128` or `len > 1048576` with SERVER_ERROR.

## EXCHANGE_RESULTS JSON (both directions)

```json
{
  "cpu_util_total": 0, "cpu_util_user": 0, "cpu_util_system": 0,
  "sender_has_retransmits": -1,   // -1: receiver or unknown (netsu-ts always -1... see note)
  "streams": [
    {
      "id": 1,                    // 1-based
      "bytes": 123456,            // sender: bytes sent; receiver: bytes received
      "retransmits": -1,          // -1 when not available
      "jitter": 0.0012,           // seconds (UDP receiver), 0 otherwise
      "errors": 0,                // UDP lost packet count
      "omitted_errors": 0,
      "packets": 8000,            // UDP packet count, 0 for TCP
      "omitted_packets": 0,
      "start_time": 0,
      "end_time": 10.0004
    }
  ]
}
```

Note: pure Node cannot read TCP_INFO, so netsu-ts sends
`sender_has_retransmits: 0` when sending, `-1` when receiving, and
`retransmits: -1` per stream. iperf3 accepts this (identical to platforms
without retransmit info).

## UDP specifics

- **Stream setup** (during CREATE_STREAMS): client sends 4-byte BE
  `0x36373839` (`UDP_CONNECT_MSG`) from a fresh (optionally connected) UDP
  socket to the server port. Server `recvfrom`s it, `connect()`s that socket
  to the peer (kernel pins the 4-tuple), binds a NEW listening socket with
  SO_REUSEADDR on the same port for subsequent streams, then replies 4-byte
  BE `0x39383736` (`UDP_CONNECT_REPLY`). Client also accepts legacy reply
  `987654321` (BE).
- **Packet header** (32-bit counters, netsu never negotiates 64-bit):
  `sec(u32 BE) | usec(u32 BE) | pcount(u32 BE)` at offset 0; rest of the
  datagram is filler. `pcount` starts at 1.
- **Receiver stats**: `lost = max_pcount - received_count` (clamped ≥ 0);
  a packet with `pcount <= max_seen` counts as out-of-order; jitter per
  RFC 1889: `transit = arrival - sent; d = |transit - prev_transit|;
  jitter += (d - jitter) / 16` (seconds).
- **Pacing**: token bucket at `bandwidth` bits/s (default 1 Mbit/s), checked
  every send.

## WebSocket mode [netsu extension]

WS binary frames are a byte pipe: the byte sequence on a WS control/data
channel is identical to the TCP byte sequence (cookie, state bytes,
length-prefixed JSON, payload). Control and each data stream use separate WS
connections to `ws://host:port/`. Fragmentation of the byte stream across WS
messages is arbitrary; receivers must reassemble. A netsu server runs in
either tcp mode or ws mode, never both on one port. Official iperf3 cannot
connect to a ws-mode server; that is expected.

## Error behavior

- Server busy → `ACCESS_DENIED (-1)` then close (client maps to "server busy").
- Malformed cookie/JSON/state, or limits exceeded → `SERVER_ERROR (-2)` then
  close; server returns to idle.
- Control-channel timeouts: 30 s for any expected control read outside
  TEST_RUNNING; during TEST_RUNNING the server caps the test at
  `time + 10 s` as a safety net.
````

- [ ] **Step 2: Commit**

```bash
cd /Users/hk/Dev/netsu
git add PROTOCOL.md && git commit -m "docs: document iperf3 wire protocol + netsu ws extension"
```

---

### Task 3: Protocol core — states, cookie, byte pipe, framing

Pure logic, no sockets. `MemoryPipe` is the in-memory test double every later unit test uses.

**Files:**
- Create: `packages/netsu/src/protocol/states.ts`
- Create: `packages/netsu/src/protocol/cookie.ts`
- Create: `packages/netsu/src/protocol/pipe.ts`
- Create: `packages/netsu/src/protocol/framing.ts`
- Test: `packages/netsu/tests/protocol.test.ts`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `states.ts`: `const TEST_START=1, TEST_RUNNING=2, TEST_END=4, PARAM_EXCHANGE=9, CREATE_STREAMS=10, SERVER_TERMINATE=11, CLIENT_TERMINATE=12, EXCHANGE_RESULTS=13, DISPLAY_RESULTS=14, IPERF_START=15, IPERF_DONE=16, ACCESS_DENIED=-1, SERVER_ERROR=-2, COOKIE_SIZE=37`
  - `cookie.ts`: `makeCookie(): string` (36 chars), `cookieToBytes(c: string): Uint8Array` (37B), `bytesToCookie(b: Uint8Array): string`
  - `pipe.ts`: `interface BytePipe { readExact(n: number, timeoutMs?: number): Promise<Uint8Array>; write(data: Uint8Array): Promise<void>; close(): void }`, `class MemoryPipe implements BytePipe` with `static pair(): [MemoryPipe, MemoryPipe]`
  - `framing.ts`: `writeState(pipe, state): Promise<void>`, `readState(pipe, timeoutMs?): Promise<number>` (signed), `writeJson(pipe, value): Promise<void>`, `readJson(pipe, maxSize?, timeoutMs?): Promise<unknown>` (default maxSize 65536)

- [ ] **Step 1: Write the failing tests**

`packages/netsu/tests/protocol.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { ACCESS_DENIED, COOKIE_SIZE, PARAM_EXCHANGE } from "../src/protocol/states.ts";
import { bytesToCookie, cookieToBytes, makeCookie } from "../src/protocol/cookie.ts";
import { MemoryPipe } from "../src/protocol/pipe.ts";
import { readJson, readState, writeJson, writeState } from "../src/protocol/framing.ts";

describe("cookie", () => {
  it("makes 36-char cookies from the iperf3 alphabet", () => {
    const c = makeCookie();
    expect(c).toHaveLength(36);
    expect(c).toMatch(/^[a-z234567]{36}$/);
    expect(makeCookie()).not.toBe(c);
  });

  it("round-trips through 37-byte NUL-terminated wire form", () => {
    const c = makeCookie();
    const b = cookieToBytes(c);
    expect(b).toHaveLength(COOKIE_SIZE);
    expect(b[36]).toBe(0);
    expect(bytesToCookie(b)).toBe(c);
  });
});

describe("MemoryPipe", () => {
  it("delivers written bytes to the peer, respecting chunk boundaries", async () => {
    const [a, b] = MemoryPipe.pair();
    await a.write(new Uint8Array([1, 2, 3, 4, 5]));
    expect([...(await b.readExact(2))]).toEqual([1, 2]);
    expect([...(await b.readExact(3))]).toEqual([3, 4, 5]);
  });

  it("readExact waits for enough bytes", async () => {
    const [a, b] = MemoryPipe.pair();
    const pending = b.readExact(4);
    await a.write(new Uint8Array([9]));
    await a.write(new Uint8Array([8, 7, 6]));
    expect([...(await pending)]).toEqual([9, 8, 7, 6]);
  });

  it("readExact rejects on close (EOF)", async () => {
    const [a, b] = MemoryPipe.pair();
    const pending = b.readExact(1);
    a.close();
    await expect(pending).rejects.toThrow(/closed/i);
  });
});

describe("framing", () => {
  it("round-trips positive and negative state bytes", async () => {
    const [a, b] = MemoryPipe.pair();
    await writeState(a, PARAM_EXCHANGE);
    await writeState(a, ACCESS_DENIED);
    expect(await readState(b)).toBe(PARAM_EXCHANGE);
    expect(await readState(b)).toBe(ACCESS_DENIED); // signed: 0xff → -1
  });

  it("round-trips JSON with 4-byte BE length prefix", async () => {
    const [a, b] = MemoryPipe.pair();
    const msg = { tcp: true, time: 10, parallel: 2 };
    await writeJson(a, msg);
    expect(await readJson(b)).toEqual(msg);
  });

  it("rejects JSON larger than maxSize", async () => {
    const [a, b] = MemoryPipe.pair();
    await writeJson(a, { pad: "x".repeat(100) });
    await expect(readJson(b, 50)).rejects.toThrow(/too large/i);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/hk/Dev/netsu/packages/netsu && bun run test tests/protocol.test.ts
```
Expected: FAIL — modules not found.

- [ ] **Step 3: Implement**

`packages/netsu/src/protocol/states.ts`:

```ts
// iperf3 control-channel states (iperf_api.h)
export const TEST_START = 1;
export const TEST_RUNNING = 2;
export const TEST_END = 4;
export const PARAM_EXCHANGE = 9;
export const CREATE_STREAMS = 10;
export const SERVER_TERMINATE = 11;
export const CLIENT_TERMINATE = 12;
export const EXCHANGE_RESULTS = 13;
export const DISPLAY_RESULTS = 14;
export const IPERF_START = 15;
export const IPERF_DONE = 16;
export const ACCESS_DENIED = -1;
export const SERVER_ERROR = -2;
export const COOKIE_SIZE = 37;
```

`packages/netsu/src/protocol/cookie.ts`:

```ts
import { randomBytes } from "node:crypto";
import { COOKIE_SIZE } from "./states.ts";

const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";

/** 36 random chars from iperf3's cookie alphabet (make_cookie in iperf_util.c). */
export function makeCookie(): string {
  const raw = randomBytes(COOKIE_SIZE - 1);
  let out = "";
  for (const byte of raw) out += ALPHABET[byte % ALPHABET.length];
  return out;
}

/** Wire form: 36 chars + NUL = 37 bytes. */
export function cookieToBytes(cookie: string): Uint8Array {
  const bytes = new Uint8Array(COOKIE_SIZE);
  new TextEncoder().encodeInto(cookie, bytes);
  return bytes;
}

export function bytesToCookie(bytes: Uint8Array): string {
  const end = bytes.indexOf(0);
  return new TextDecoder().decode(bytes.subarray(0, end === -1 ? bytes.length : end));
}
```

`packages/netsu/src/protocol/pipe.ts`:

```ts
/** Transport-agnostic ordered byte stream. Control channels always speak this. */
export interface BytePipe {
  /** Resolve with exactly n bytes; reject on EOF/close/timeout. */
  readExact(n: number, timeoutMs?: number): Promise<Uint8Array>;
  /** Resolve when the bytes are handed to the transport (backpressure point). */
  write(data: Uint8Array): Promise<void>;
  close(): void;
}

interface Waiter {
  n: number;
  resolve: (b: Uint8Array) => void;
  reject: (e: Error) => void;
  timer?: ReturnType<typeof setTimeout>;
}

/** Shared buffering logic: transports feed bytes in, readExact pulls them out. */
export class ByteBuffer {
  private chunks: Uint8Array[] = [];
  private length = 0;
  private waiter: Waiter | undefined;
  private closed = false;

  feed(data: Uint8Array): void {
    this.chunks.push(data);
    this.length += data.length;
    this.pump();
  }

  end(): void {
    this.closed = true;
    if (this.waiter) {
      const w = this.waiter;
      this.waiter = undefined;
      if (w.timer) clearTimeout(w.timer);
      w.reject(new Error("pipe closed"));
    }
  }

  get buffered(): number {
    return this.length;
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    if (this.waiter) return Promise.reject(new Error("concurrent readExact"));
    return new Promise((resolve, reject) => {
      const waiter: Waiter = { n, resolve, reject };
      if (timeoutMs !== undefined) {
        waiter.timer = setTimeout(() => {
          this.waiter = undefined;
          reject(new Error(`read timeout after ${timeoutMs}ms`));
        }, timeoutMs);
      }
      this.waiter = waiter;
      if (this.closed && this.length < n) return this.end();
      this.pump();
    });
  }

  private pump(): void {
    const w = this.waiter;
    if (!w || this.length < w.n) return;
    this.waiter = undefined;
    if (w.timer) clearTimeout(w.timer);
    const out = new Uint8Array(w.n);
    let offset = 0;
    while (offset < w.n) {
      const head = this.chunks[0]!;
      const take = Math.min(head.length, w.n - offset);
      out.set(head.subarray(0, take), offset);
      offset += take;
      if (take === head.length) this.chunks.shift();
      else this.chunks[0] = head.subarray(take);
    }
    this.length -= w.n;
    w.resolve(out);
  }
}

/** In-memory pipe pair for unit tests. */
export class MemoryPipe implements BytePipe {
  private buffer = new ByteBuffer();
  private peer!: MemoryPipe;

  static pair(): [MemoryPipe, MemoryPipe] {
    const a = new MemoryPipe();
    const b = new MemoryPipe();
    a.peer = b;
    b.peer = a;
    return [a, b];
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    return this.buffer.readExact(n, timeoutMs);
  }

  async write(data: Uint8Array): Promise<void> {
    this.peer.buffer.feed(data.slice());
  }

  close(): void {
    this.buffer.end();
    this.peer.buffer.end();
  }
}
```

`packages/netsu/src/protocol/framing.ts`:

```ts
import type { BytePipe } from "./pipe.ts";

/** Single signed state byte (iperf3 writes states as one byte on the control channel). */
export async function writeState(pipe: BytePipe, state: number): Promise<void> {
  await pipe.write(new Uint8Array([state & 0xff]));
}

export async function readState(pipe: BytePipe, timeoutMs?: number): Promise<number> {
  const b = await pipe.readExact(1, timeoutMs);
  return (b[0]! << 24) >> 24; // sign-extend
}

/** [u32 BE length][UTF-8 JSON] — JSON_write in iperf_api.c. */
export async function writeJson(pipe: BytePipe, value: unknown): Promise<void> {
  const body = new TextEncoder().encode(JSON.stringify(value));
  const frame = new Uint8Array(4 + body.length);
  new DataView(frame.buffer).setUint32(0, body.length);
  frame.set(body, 4);
  await pipe.write(frame);
}

export async function readJson(
  pipe: BytePipe,
  maxSize = 65536,
  timeoutMs?: number,
): Promise<unknown> {
  const head = await pipe.readExact(4, timeoutMs);
  const size = new DataView(head.buffer, head.byteOffset).getUint32(0);
  if (size === 0 || size > maxSize) {
    throw new Error(`json frame too large: ${size} > ${maxSize}`);
  }
  const body = await pipe.readExact(size, timeoutMs);
  return JSON.parse(new TextDecoder().decode(body));
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
bun run test tests/protocol.test.ts && bun run typecheck
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: protocol core - states, cookie, byte pipe, iperf3 framing"
```

---

### Task 4: Params and results codecs

Typed encode/decode for the two JSON messages, field names exactly as PROTOCOL.md.

**Files:**
- Create: `packages/netsu/src/protocol/params.ts`
- Create: `packages/netsu/src/protocol/results.ts`
- Test: `packages/netsu/tests/codecs.test.ts`

**Interfaces:**
- Consumes: nothing new
- Produces:
  - `params.ts`: `interface TestParams { udp: boolean; time: number; parallel: number; len: number; reverse: boolean; bandwidth: number }`, `encodeParams(p: TestParams): Record<string, unknown>`, `decodeParams(v: unknown): TestParams` (throws on invalid / limits: parallel ≤ 128, len ≤ 1 MiB), `DEFAULT_TCP_LEN = 131072`, `DEFAULT_UDP_LEN = 1460`, `DEFAULT_UDP_BANDWIDTH = 1048576`
  - `results.ts`: `interface StreamResult { id: number; bytes: number; retransmits: number; jitter: number; errors: number; packets: number; startTime: number; endTime: number }`, `interface EndResults { senderHasRetransmits: number; streams: StreamResult[] }`, `encodeResults(r: EndResults): Record<string, unknown>`, `decodeResults(v: unknown): EndResults`

- [ ] **Step 1: Write the failing tests**

`packages/netsu/tests/codecs.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { decodeParams, encodeParams, type TestParams } from "../src/protocol/params.ts";
import { decodeResults, encodeResults, type EndResults } from "../src/protocol/results.ts";

const params: TestParams = {
  udp: false, time: 10, parallel: 2, len: 131072, reverse: true, bandwidth: 0,
};

describe("params codec", () => {
  it("encodes iperf3 field names", () => {
    const j = encodeParams(params);
    expect(j).toMatchObject({ tcp: true, time: 10, parallel: 2, len: 131072, reverse: true });
    expect(j).not.toHaveProperty("udp");
    expect(j).not.toHaveProperty("bandwidth"); // tcp: no pacing field
    expect(j).toHaveProperty("client_version");
  });

  it("encodes udp with bandwidth, without reverse when false", () => {
    const j = encodeParams({ ...params, udp: true, reverse: false, bandwidth: 1048576, len: 1460 });
    expect(j).toMatchObject({ udp: true, bandwidth: 1048576, len: 1460 });
    expect(j).not.toHaveProperty("tcp");
    expect(j).not.toHaveProperty("reverse");
  });

  it("decodes its own output and tolerates unknown fields", () => {
    const decoded = decodeParams({ ...encodeParams(params), MSS: 1400, congestion: "cubic" });
    expect(decoded).toEqual(params);
  });

  it("rejects out-of-bounds values", () => {
    expect(() => decodeParams({ tcp: true, time: 10, parallel: 500, len: 1000 })).toThrow();
    expect(() => decodeParams({ tcp: true, time: 10, parallel: 1, len: 99999999 })).toThrow();
    expect(() => decodeParams({ time: 10 })).toThrow(); // neither tcp nor udp
  });
});

describe("results codec", () => {
  const results: EndResults = {
    senderHasRetransmits: -1,
    streams: [
      { id: 1, bytes: 5000, retransmits: -1, jitter: 0.002, errors: 3, packets: 100, startTime: 0, endTime: 10.01 },
    ],
  };

  it("round-trips through iperf3 field names", () => {
    const j = encodeResults(results);
    expect(j).toMatchObject({ cpu_util_total: 0, sender_has_retransmits: -1 });
    const s = (j as { streams: Record<string, unknown>[] }).streams[0]!;
    expect(s).toMatchObject({ id: 1, bytes: 5000, jitter: 0.002, errors: 3, packets: 100, start_time: 0, end_time: 10.01 });
    expect(decodeResults(j)).toEqual(results);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bun run test tests/codecs.test.ts
```
Expected: FAIL — modules not found.

- [ ] **Step 3: Implement**

`packages/netsu/src/protocol/params.ts`:

```ts
import * as v from "valibot";

export const DEFAULT_TCP_LEN = 131072;
export const DEFAULT_UDP_LEN = 1460;
export const DEFAULT_UDP_BANDWIDTH = 1048576; // 1 Mbit/s, iperf3's UDP default
export const MAX_PARALLEL = 128;
export const MAX_LEN = 1048576;

export interface TestParams {
  udp: boolean;
  time: number;
  parallel: number;
  len: number;
  reverse: boolean;
  bandwidth: number; // bits/s; 0 = unpaced (TCP)
}

/** PARAM_EXCHANGE payload, field names from iperf3 send_parameters(). */
export function encodeParams(p: TestParams): Record<string, unknown> {
  return {
    ...(p.udp ? { udp: true } : { tcp: true }),
    omit: 0,
    time: p.time,
    num: 0,
    blockcount: 0,
    parallel: p.parallel,
    ...(p.reverse ? { reverse: true } : {}),
    len: p.len,
    ...(p.udp ? { bandwidth: p.bandwidth } : {}),
    pacing_timer: 1000,
    client_version: "netsu-0.2.0",
  };
}

const WireParams = v.looseObject({
  tcp: v.optional(v.boolean()),
  udp: v.optional(v.boolean()),
  time: v.pipe(v.number(), v.minValue(1), v.maxValue(86400)),
  parallel: v.pipe(v.number(), v.integer(), v.minValue(1), v.maxValue(MAX_PARALLEL)),
  reverse: v.optional(v.boolean()),
  len: v.pipe(v.number(), v.integer(), v.minValue(4), v.maxValue(MAX_LEN)),
  bandwidth: v.optional(v.pipe(v.number(), v.minValue(0))),
});

export function decodeParams(value: unknown): TestParams {
  const p = v.parse(WireParams, value);
  if (!p.tcp && !p.udp) throw new Error("params: neither tcp nor udp");
  return {
    udp: p.udp === true,
    time: p.time,
    parallel: p.parallel,
    len: p.len,
    reverse: p.reverse === true,
    bandwidth: p.bandwidth ?? 0,
  };
}
```

`packages/netsu/src/protocol/results.ts`:

```ts
import * as v from "valibot";

export interface StreamResult {
  id: number;
  bytes: number;
  retransmits: number;
  jitter: number; // seconds
  errors: number; // UDP lost packets
  packets: number;
  startTime: number;
  endTime: number;
}

export interface EndResults {
  senderHasRetransmits: number;
  streams: StreamResult[];
}

/** EXCHANGE_RESULTS payload, field names from iperf3 send_results(). */
export function encodeResults(r: EndResults): Record<string, unknown> {
  return {
    cpu_util_total: 0,
    cpu_util_user: 0,
    cpu_util_system: 0,
    sender_has_retransmits: r.senderHasRetransmits,
    streams: r.streams.map((s) => ({
      id: s.id,
      bytes: s.bytes,
      retransmits: s.retransmits,
      jitter: s.jitter,
      errors: s.errors,
      omitted_errors: 0,
      packets: s.packets,
      omitted_packets: 0,
      start_time: s.startTime,
      end_time: s.endTime,
    })),
  };
}

const WireStream = v.looseObject({
  id: v.number(),
  bytes: v.number(),
  retransmits: v.optional(v.number(), -1),
  jitter: v.optional(v.number(), 0),
  errors: v.optional(v.number(), 0),
  packets: v.optional(v.number(), 0),
  start_time: v.optional(v.number(), 0),
  end_time: v.optional(v.number(), 0),
});

const WireResults = v.looseObject({
  sender_has_retransmits: v.optional(v.number(), -1),
  streams: v.array(WireStream),
});

export function decodeResults(value: unknown): EndResults {
  const r = v.parse(WireResults, value);
  return {
    senderHasRetransmits: r.sender_has_retransmits,
    streams: r.streams.map((s) => ({
      id: s.id,
      bytes: s.bytes,
      retransmits: s.retransmits,
      jitter: s.jitter,
      errors: s.errors,
      packets: s.packets,
      startTime: s.start_time,
      endTime: s.end_time,
    })),
  };
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
bun run test tests/codecs.test.ts && bun run typecheck
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: params and results codecs with iperf3 field names"
```

---

### Task 5: Stats — interval meter and UDP jitter/loss tracker

Pure math. The jitter test uses a hand-computed RFC 1889 sequence — do not change the expected values to make the test pass; fix the implementation.

**Files:**
- Create: `packages/netsu/src/stats.ts`
- Test: `packages/netsu/tests/stats.test.ts`

**Interfaces:**
- Consumes: nothing new
- Produces:
  - `bitsPerSecond(bytes: number, seconds: number): number` (0 when seconds ≤ 0)
  - `class IntervalMeter { add(bytes: number): void; snap(nowMs: number): IntervalReport; readonly totalBytes: number }` — construct with `new IntervalMeter(startMs)`
  - `interface IntervalReport { start: number; end: number; bytes: number; bitsPerSecond: number }` (start/end in seconds since test start)
  - `class JitterTracker { onPacket(pcount: number, sentMs: number, nowMs: number): void; readonly jitterMs: number; readonly lost: number; readonly outOfOrder: number; readonly received: number; readonly maxSeq: number }`

- [ ] **Step 1: Write the failing tests**

`packages/netsu/tests/stats.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { IntervalMeter, JitterTracker, bitsPerSecond } from "../src/stats.ts";

describe("bitsPerSecond", () => {
  it("converts bytes over seconds to bits/s", () => {
    expect(bitsPerSecond(1_000_000, 8)).toBe(1_000_000);
    expect(bitsPerSecond(100, 0)).toBe(0);
  });
});

describe("IntervalMeter", () => {
  it("reports per-interval deltas and running total", () => {
    const m = new IntervalMeter(1000);
    m.add(500);
    m.add(500);
    const first = m.snap(2000); // 1s later
    expect(first).toEqual({ start: 0, end: 1, bytes: 1000, bitsPerSecond: 8000 });
    m.add(250);
    const second = m.snap(3000);
    expect(second.start).toBe(1);
    expect(second.bytes).toBe(250);
    expect(m.totalBytes).toBe(1250);
  });
});

describe("JitterTracker", () => {
  it("tracks loss and out-of-order from packet counts", () => {
    const t = new JitterTracker();
    t.onPacket(1, 0, 10);
    t.onPacket(2, 10, 20);
    t.onPacket(5, 40, 50); // 3,4 missing
    t.onPacket(4, 30, 55); // 4 arrives late: out of order, no longer lost
    expect(t.received).toBe(4);
    expect(t.maxSeq).toBe(5);
    expect(t.outOfOrder).toBe(1);
    expect(t.lost).toBe(1); // 5 expected, 4 received
  });

  it("computes RFC1889 jitter (hand-computed sequence)", () => {
    const t = new JitterTracker();
    // transit times: 10, 12, 9 → d = 2, 3
    t.onPacket(1, 0, 10);   // first packet: jitter stays 0
    t.onPacket(2, 100, 112); // d=|12-10|=2 → jitter = 0 + (2-0)/16 = 0.125
    t.onPacket(3, 200, 209); // d=|9-12|=3 → jitter = 0.125 + (3-0.125)/16 ≈ 0.3047
    expect(t.jitterMs).toBeCloseTo(0.3047, 3);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bun run test tests/stats.test.ts
```
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

`packages/netsu/src/stats.ts`:

```ts
export function bitsPerSecond(bytes: number, seconds: number): number {
  return seconds > 0 ? (bytes * 8) / seconds : 0;
}

export interface IntervalReport {
  start: number; // seconds since test start
  end: number;
  bytes: number;
  bitsPerSecond: number;
}

/** Accumulates bytes; snap() closes the current interval and starts the next. */
export class IntervalMeter {
  #total = 0;
  #intervalBytes = 0;
  #startMs: number;
  #lastSnapMs: number;

  constructor(startMs: number) {
    this.#startMs = startMs;
    this.#lastSnapMs = startMs;
  }

  add(bytes: number): void {
    this.#total += bytes;
    this.#intervalBytes += bytes;
  }

  get totalBytes(): number {
    return this.#total;
  }

  snap(nowMs: number): IntervalReport {
    const seconds = (nowMs - this.#lastSnapMs) / 1000;
    const report: IntervalReport = {
      start: (this.#lastSnapMs - this.#startMs) / 1000,
      end: (nowMs - this.#startMs) / 1000,
      bytes: this.#intervalBytes,
      bitsPerSecond: bitsPerSecond(this.#intervalBytes, seconds),
    };
    this.#lastSnapMs = nowMs;
    this.#intervalBytes = 0;
    return report;
  }
}

/** RFC 1889 jitter + loss/reorder accounting for UDP receive side. */
export class JitterTracker {
  #jitterMs = 0;
  #prevTransit: number | undefined;
  #maxSeq = 0;
  #received = 0;
  #outOfOrder = 0;

  onPacket(pcount: number, sentMs: number, nowMs: number): void {
    this.#received++;
    if (pcount > this.#maxSeq) this.#maxSeq = pcount;
    else this.#outOfOrder++;

    const transit = nowMs - sentMs;
    if (this.#prevTransit !== undefined) {
      const d = Math.abs(transit - this.#prevTransit);
      this.#jitterMs += (d - this.#jitterMs) / 16;
    }
    this.#prevTransit = transit;
  }

  get jitterMs(): number {
    return this.#jitterMs;
  }
  get received(): number {
    return this.#received;
  }
  get maxSeq(): number {
    return this.#maxSeq;
  }
  get outOfOrder(): number {
    return this.#outOfOrder;
  }
  /** Expected (highest seq) minus received; late arrivals reduce loss. */
  get lost(): number {
    return Math.max(0, this.#maxSeq - this.#received);
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
bun run test tests/stats.test.ts && bun run typecheck
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: interval meter and rfc1889 jitter/loss tracker"
```

---

### Task 6: TCP transport — BytePipe over net.Socket + data channel

Two wrappers around one socket: `TcpPipe` (control handshakes, pull-based) and `TcpDataChannel` (bulk payload, push-based, backpressure-correct). A socket starts life under `TcpPipe`; `detach()` hands it to a `TcpDataChannel` once cookies are exchanged.

**Files:**
- Create: `packages/netsu/src/transport/tcp.ts`
- Create: `packages/netsu/src/streams/channel.ts`
- Test: `packages/netsu/tests/tcp-transport.test.ts`

**Interfaces:**
- Consumes: `BytePipe`, `ByteBuffer` from `protocol/pipe.ts`
- Produces:
  - `channel.ts`: `interface DataChannel { write(chunk: Uint8Array): Promise<void>; onData(cb: (byteLength: number) => void): void; close(): void }`
  - `tcp.ts`: `class TcpPipe implements BytePipe` with `readonly socket: net.Socket` and `detach(): net.Socket` (removes listeners; throws if bytes are buffered — protocol guarantees none), `tcpConnect(host: string, port: number): Promise<TcpPipe>`, `class TcpDataChannel implements DataChannel` (constructor takes a detached `net.Socket`; `write` resolves on drain when the kernel buffer is full)

- [ ] **Step 1: Write the failing tests**

`packages/netsu/tests/tcp-transport.test.ts`:

```ts
import { createServer, type Socket } from "node:net";
import { afterEach, describe, expect, it } from "vitest";
import { TcpDataChannel, TcpPipe, tcpConnect } from "../src/transport/tcp.ts";
import { readJson, writeJson } from "../src/protocol/framing.ts";

const cleanups: (() => void)[] = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

function listen(onConn: (s: Socket) => void): Promise<number> {
  const server = createServer(onConn);
  cleanups.push(() => server.close());
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve((server.address() as { port: number }).port);
    });
  });
}

describe("TcpPipe", () => {
  it("carries framed json both ways", async () => {
    const port = await listen(async (s) => {
      const pipe = new TcpPipe(s);
      const msg = await readJson(pipe);
      await writeJson(pipe, { echo: msg });
    });
    const pipe = await tcpConnect("127.0.0.1", port);
    cleanups.push(() => pipe.close());
    await writeJson(pipe, { hello: 1 });
    expect(await readJson(pipe)).toEqual({ echo: { hello: 1 } });
  });

  it("detach hands over a clean socket for bulk transfer", async () => {
    // Mirrors the protocol's gating: the receiver acks the handshake before
    // the sender starts bulk data, so no payload can coalesce with the
    // handshake bytes and detach() always sees an empty buffer.
    const received: number[] = [];
    let done!: () => void;
    const finished = new Promise<void>((r) => (done = r));
    const port = await listen(async (s) => {
      const pipe = new TcpPipe(s);
      await pipe.readExact(4); // handshake (cookie stand-in)
      await pipe.write(new Uint8Array([1])); // ack — the TEST_START stand-in
      const channel = new TcpDataChannel(pipe.detach());
      channel.onData((n) => {
        received.push(n);
        if (received.reduce((a, b) => a + b, 0) >= 65536) done();
      });
    });
    const pipe = await tcpConnect("127.0.0.1", port);
    cleanups.push(() => pipe.close());
    await pipe.write(new Uint8Array([1, 2, 3, 4]));
    await pipe.readExact(1); // wait for ack before sending bulk
    const channel = new TcpDataChannel(pipe.detach());
    await channel.write(new Uint8Array(65536).fill(7));
    await finished;
    expect(received.reduce((a, b) => a + b, 0)).toBeGreaterThanOrEqual(65536);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bun run test tests/tcp-transport.test.ts
```
Expected: FAIL — modules not found.

- [ ] **Step 3: Implement**

`packages/netsu/src/streams/channel.ts`:

```ts
/** Bulk payload channel for data streams (TCP/WS). UDP is packet-based and separate. */
export interface DataChannel {
  /** Backpressure point: resolves when the transport can take more. */
  write(chunk: Uint8Array): Promise<void>;
  onData(cb: (byteLength: number) => void): void;
  close(): void;
}
```

`packages/netsu/src/transport/tcp.ts`:

```ts
import { Socket, connect } from "node:net";
import { ByteBuffer, type BytePipe } from "../protocol/pipe.ts";
import type { DataChannel } from "../streams/channel.ts";

/** Control-channel view of a TCP socket. */
export class TcpPipe implements BytePipe {
  readonly socket: Socket;
  #buffer = new ByteBuffer();
  #onData = (d: Buffer) => this.#buffer.feed(d);
  #onClose = () => this.#buffer.end();

  constructor(socket: Socket) {
    this.socket = socket;
    socket.on("data", this.#onData);
    socket.on("close", this.#onClose);
    socket.on("error", this.#onClose);
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    return this.#buffer.readExact(n, timeoutMs);
  }

  write(data: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      this.socket.write(data, (err) => (err ? reject(err) : resolve()));
    });
  }

  /** Stop interpreting bytes; caller takes the raw socket (for TcpDataChannel). */
  detach(): Socket {
    if (this.#buffer.buffered > 0) throw new Error("detach with buffered bytes");
    this.socket.off("data", this.#onData);
    this.socket.off("close", this.#onClose);
    this.socket.off("error", this.#onClose);
    return this.socket;
  }

  close(): void {
    this.socket.destroy();
    this.#buffer.end();
  }
}

export function tcpConnect(host: string, port: number): Promise<TcpPipe> {
  return new Promise((resolve, reject) => {
    const socket = connect({ host, port, noDelay: true }, () => {
      socket.off("error", reject);
      resolve(new TcpPipe(socket));
    });
    socket.once("error", reject);
  });
}

/** Bulk payload over a detached socket. write() honors kernel backpressure via drain. */
export class TcpDataChannel implements DataChannel {
  #socket: Socket;

  constructor(socket: Socket) {
    this.#socket = socket;
    socket.on("error", () => socket.destroy());
  }

  write(chunk: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      const ok = this.#socket.write(chunk, (err) => {
        if (err) reject(err);
      });
      if (ok) resolve();
      else this.#socket.once("drain", resolve);
    });
  }

  onData(cb: (byteLength: number) => void): void {
    this.#socket.on("data", (d: Buffer) => cb(d.length));
  }

  close(): void {
    this.#socket.destroy();
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
bun run test tests/tcp-transport.test.ts && bun run typecheck
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: tcp transport - control pipe and backpressured data channel"
```

---

### Task 7: Client — full control state machine, verified against official iperf3

The client is written once, complete with reverse and parallel support (the protocol carries them anyway). UDP and WS branches throw "not implemented" until Tasks 9/10. Verification is against a real `iperf3 -s`.

**Files:**
- Create: `packages/netsu/src/streams/runner.ts`
- Create: `packages/netsu/src/client.ts`
- Create: `packages/netsu/tests/helpers.ts`
- Test: `packages/netsu/tests/client-iperf3.test.ts`

**Interfaces:**
- Consumes: everything from Tasks 3–6
- Produces:
  - `runner.ts`: `interface StreamCounters { id: number; bytes: number; packets: number; jitter: number; errors: number }`, `attachReceiver(channel: DataChannel, counters: StreamCounters, onBytes?: (n: number) => void): void`, `startSender(channel: DataChannel, counters: StreamCounters, len: number, isRunning: () => boolean, onBytes?: (n: number) => void): Promise<void>`
  - `client.ts`: `runClient(host: string, opts?: ClientOptions): Promise<TestResult>` with `interface ClientOptions { port?; transport?: "tcp" | "ws"; udp?; reverse?; duration?; parallel?; len?; bandwidth?; interval?; onInterval? }` and `interface TestResult { udp: boolean; reverse: boolean; durationSeconds: number; sentBytes: number; receivedBytes: number; sendBitsPerSecond: number; receiveBitsPerSecond: number; local: EndResults; remote: EndResults; udpStats?: UdpStats }`, `interface UdpStats { jitterMs: number; lost: number; packets: number; lostPercent: number }`
  - `helpers.ts`: `HAS_IPERF3: boolean`, `spawnIperf3Server(port: number, extra?: string[]): Promise<() => void>`, `runIperf3Client(args: string[]): Promise<{ code: number; json: unknown }>`, `nextPort(): number`

- [ ] **Step 1: Write the test helpers**

`packages/netsu/tests/helpers.ts`:

```ts
import { execSync, spawn } from "node:child_process";

export const HAS_IPERF3 = (() => {
  try {
    execSync("iperf3 --version", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

let portCounter = 5210;
/** Unique port per test — never 5201, see global constraints. */
export function nextPort(): number {
  return portCounter++;
}

/** Spawn `iperf3 -s -1` (one-off server); resolves once it is listening. */
export function spawnIperf3Server(port: number, extra: string[] = []): Promise<() => void> {
  const proc = spawn("iperf3", ["-s", "-1", "-p", String(port), ...extra], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("iperf3 -s did not start")), 5000);
    proc.stdout.on("data", (d: Buffer) => {
      if (d.toString().includes("Server listening")) {
        clearTimeout(timer);
        resolve(() => proc.kill("SIGKILL"));
      }
    });
    proc.on("error", reject);
  });
}

/** Run `iperf3 -c ... --json`, return exit code and parsed output. */
export function runIperf3Client(args: string[]): Promise<{ code: number; json: unknown }> {
  const proc = spawn("iperf3", [...args, "--json"], { stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  proc.stdout.on("data", (d: Buffer) => (out += d.toString()));
  return new Promise((resolve, reject) => {
    proc.on("error", reject);
    proc.on("close", (code) => {
      try {
        resolve({ code: code ?? -1, json: JSON.parse(out) });
      } catch {
        reject(new Error(`iperf3 output not json (exit ${code}): ${out.slice(0, 300)}`));
      }
    });
  });
}
```

- [ ] **Step 2: Write the failing integration tests**

`packages/netsu/tests/client-iperf3.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { HAS_IPERF3, nextPort, spawnIperf3Server } from "./helpers.ts";

describe.skipIf(!HAS_IPERF3)("netsu client vs official iperf3 server (tcp)", () => {
  it("upload: transfers and exchanges results", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, duration: 2 });
      expect(r.reverse).toBe(false);
      expect(r.sentBytes).toBeGreaterThan(1_000_000); // loopback: way more in 2s
      expect(r.receivedBytes).toBeGreaterThan(0);
      expect(r.receivedBytes).toBeLessThanOrEqual(r.sentBytes * 1.01);
      expect(r.sendBitsPerSecond).toBeGreaterThan(1_000_000);
    } finally {
      kill();
    }
  }, 15000);

  it("reverse (-R): receives from server", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, duration: 2, reverse: true });
      expect(r.receivedBytes).toBeGreaterThan(1_000_000);
      expect(r.local.senderHasRetransmits).toBe(-1); // we are the receiver
    } finally {
      kill();
    }
  }, 15000);

  it("parallel (-P 3): three streams with per-stream results", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, duration: 2, parallel: 3 });
      expect(r.local.streams).toHaveLength(3);
      expect(r.remote.streams).toHaveLength(3);
      for (const s of r.local.streams) expect(s.bytes).toBeGreaterThan(0);
    } finally {
      kill();
    }
  }, 15000);

  it("reports intervals roughly every second", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const reports: number[] = [];
      await runClient("127.0.0.1", {
        port, duration: 3,
        onInterval: (rep) => reports.push(rep.bitsPerSecond),
      });
      expect(reports.length).toBeGreaterThanOrEqual(2);
      for (const bps of reports) expect(bps).toBeGreaterThan(0);
    } finally {
      kill();
    }
  }, 15000);
});
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
bun run test tests/client-iperf3.test.ts
```
Expected: FAIL — `src/client.ts` not found.

- [ ] **Step 4: Implement runner and client**

`packages/netsu/src/streams/runner.ts`:

```ts
import { randomBytes } from "node:crypto";
import type { DataChannel } from "./channel.ts";

/** Mutable per-stream accounting shared by client and server. */
export interface StreamCounters {
  id: number;
  bytes: number;
  packets: number;
  jitter: number; // seconds
  errors: number;
}

export function makeCounters(id: number): StreamCounters {
  return { id, bytes: 0, packets: 0, jitter: 0, errors: 0 };
}

export function attachReceiver(
  channel: DataChannel,
  counters: StreamCounters,
  onBytes?: (n: number) => void,
): void {
  channel.onData((n) => {
    counters.bytes += n;
    onBytes?.(n);
  });
}

/** Send random data (defeats link compression) until isRunning() is false. */
export async function startSender(
  channel: DataChannel,
  counters: StreamCounters,
  len: number,
  isRunning: () => boolean,
  onBytes?: (n: number) => void,
): Promise<void> {
  const chunk = randomBytes(len);
  try {
    while (isRunning()) {
      await channel.write(chunk);
      counters.bytes += chunk.length;
      onBytes?.(chunk.length);
    }
  } catch {
    // channel torn down at test end — expected
  }
}
```

`packages/netsu/src/client.ts`:

```ts
import { cookieToBytes, makeCookie } from "./protocol/cookie.ts";
import { readJson, readState, writeJson, writeState } from "./protocol/framing.ts";
import type { BytePipe } from "./protocol/pipe.ts";
import {
  DEFAULT_TCP_LEN, DEFAULT_UDP_BANDWIDTH, DEFAULT_UDP_LEN,
  encodeParams, type TestParams,
} from "./protocol/params.ts";
import { decodeResults, encodeResults, type EndResults } from "./protocol/results.ts";
import {
  ACCESS_DENIED, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS, IPERF_DONE,
  IPERF_START, PARAM_EXCHANGE, SERVER_ERROR, TEST_END, TEST_RUNNING, TEST_START,
} from "./protocol/states.ts";
import { bitsPerSecond, IntervalMeter, type IntervalReport } from "./stats.ts";
import { attachReceiver, makeCounters, startSender, type StreamCounters } from "./streams/runner.ts";
import { TcpDataChannel, tcpConnect } from "./transport/tcp.ts";

export interface ClientOptions {
  port?: number;
  transport?: "tcp" | "ws";
  udp?: boolean;
  reverse?: boolean;
  duration?: number; // seconds, default 10
  parallel?: number; // default 1
  len?: number; // blksize
  bandwidth?: number; // bits/s, UDP pacing
  interval?: number; // seconds between onInterval calls; 0 disables
  onInterval?: (report: IntervalReport) => void;
}

export interface UdpStats {
  jitterMs: number;
  lost: number;
  packets: number;
  lostPercent: number;
}

export interface TestResult {
  udp: boolean;
  reverse: boolean;
  durationSeconds: number;
  sentBytes: number;
  receivedBytes: number;
  sendBitsPerSecond: number;
  receiveBitsPerSecond: number;
  local: EndResults;
  remote: EndResults;
  udpStats?: UdpStats;
}

const CONTROL_TIMEOUT = 30_000;

interface StreamHandle {
  counters: StreamCounters;
  start(): void;
  /** Copy async trackers (e.g. UDP jitter) into counters before results. */
  finalize(): void;
  close(): void;
}

export async function runClient(host: string, opts: ClientOptions = {}): Promise<TestResult> {
  const udp = opts.udp ?? false;
  const params: TestParams = {
    udp,
    time: opts.duration ?? 10,
    parallel: opts.parallel ?? 1,
    len: opts.len ?? (udp ? DEFAULT_UDP_LEN : DEFAULT_TCP_LEN),
    reverse: opts.reverse ?? false,
    bandwidth: opts.bandwidth ?? (udp ? DEFAULT_UDP_BANDWIDTH : 0),
  };
  const session = new ClientSession(host, opts.port ?? 5201, opts.transport ?? "tcp", params, opts);
  return session.run();
}

class ClientSession {
  readonly cookie = makeCookie();
  #streams: StreamHandle[] = [];
  #meter = new IntervalMeter(Date.now());
  #running = false;
  #startMs = 0;
  #endMs = 0;
  #remote: EndResults | undefined;
  #endTimer: ReturnType<typeof setTimeout> | undefined;
  #intervalTimer: ReturnType<typeof setInterval> | undefined;

  constructor(
    private host: string,
    private port: number,
    private transport: "tcp" | "ws",
    private params: TestParams,
    private opts: ClientOptions,
  ) {}

  async run(): Promise<TestResult> {
    const control = await this.#connectControl();
    try {
      await control.write(cookieToBytes(this.cookie));
      for (;;) {
        const timeout = this.#running
          ? this.params.time * 1000 + CONTROL_TIMEOUT
          : CONTROL_TIMEOUT;
        const state = await readState(control, timeout);
        switch (state) {
          case IPERF_START:
            break; // informational, ignore
          case PARAM_EXCHANGE:
            await writeJson(control, encodeParams(this.params));
            break;
          case CREATE_STREAMS:
            for (let id = 1; id <= this.params.parallel; id++) {
              this.#streams.push(await this.#openStream(id));
            }
            break;
          case TEST_START:
            break; // streams already open; wait for TEST_RUNNING
          case TEST_RUNNING:
            this.#startRunning(control);
            break;
          case EXCHANGE_RESULTS: {
            for (const s of this.#streams) s.finalize();
            await writeJson(control, encodeResults(this.#localResults()));
            this.#remote = decodeResults(await readJson(control, 65536, CONTROL_TIMEOUT));
            break;
          }
          case DISPLAY_RESULTS:
            await writeState(control, IPERF_DONE);
            return this.#buildResult();
          case ACCESS_DENIED:
            throw new Error("server busy (ACCESS_DENIED)");
          case SERVER_ERROR:
            throw new Error("server reported error (SERVER_ERROR)");
          default:
            throw new Error(`unexpected control state ${state}`);
        }
      }
    } finally {
      this.#cleanup(control);
    }
  }

  #connectControl(): Promise<BytePipe> {
    if (this.transport === "tcp") return tcpConnect(this.host, this.port);
    throw new Error("ws transport wired in a later task"); // Task 10 replaces this line
  }

  async #openStream(id: number): Promise<StreamHandle> {
    if (this.params.udp) throw new Error("udp wired in a later task"); // Task 9 replaces this line
    if (this.transport === "ws") throw new Error("ws wired in a later task"); // Task 10 replaces this line
    return this.#openTcpStream(id);
  }

  async #openTcpStream(id: number): Promise<StreamHandle> {
    const pipe = await tcpConnect(this.host, this.port);
    await pipe.write(cookieToBytes(this.cookie));
    const channel = new TcpDataChannel(pipe.detach());
    const counters = makeCounters(id);
    if (this.params.reverse) {
      attachReceiver(channel, counters, (n) => this.#meter.add(n));
    }
    return {
      counters,
      start: () => {
        if (!this.params.reverse) {
          void startSender(channel, counters, this.params.len, () => this.#running, (n) =>
            this.#meter.add(n),
          );
        }
      },
      finalize: () => {},
      close: () => channel.close(),
    };
  }

  #startRunning(control: BytePipe): void {
    this.#running = true;
    this.#startMs = Date.now();
    this.#meter = new IntervalMeter(this.#startMs);
    for (const s of this.#streams) s.start();

    const intervalSec = this.opts.interval ?? 1;
    if (intervalSec > 0 && this.opts.onInterval) {
      this.#intervalTimer = setInterval(() => {
        this.opts.onInterval?.(this.#meter.snap(Date.now()));
      }, intervalSec * 1000);
    }

    this.#endTimer = setTimeout(() => {
      this.#running = false;
      this.#endMs = Date.now();
      if (this.#intervalTimer) clearInterval(this.#intervalTimer);
      void writeState(control, TEST_END).catch(() => {});
    }, this.params.time * 1000);
  }

  #localResults(): EndResults {
    const sender = !this.params.reverse;
    const endSeconds = (this.#endMs - this.#startMs) / 1000;
    return {
      senderHasRetransmits: sender ? 0 : -1,
      streams: this.#streams.map(({ counters }) => ({
        id: counters.id,
        bytes: counters.bytes,
        retransmits: -1, // no TCP_INFO from pure Node — see PROTOCOL.md
        jitter: counters.jitter,
        errors: counters.errors,
        packets: counters.packets,
        startTime: 0,
        endTime: endSeconds,
      })),
    };
  }

  #buildResult(): TestResult {
    const local = this.#localResults();
    const remote = this.#remote;
    if (!remote) throw new Error("no results from server");
    const duration = (this.#endMs - this.#startMs) / 1000;
    const sum = (r: EndResults) => r.streams.reduce((a, s) => a + s.bytes, 0);
    const sender = !this.params.reverse;
    const sentBytes = sender ? sum(local) : sum(remote);
    const receivedBytes = sender ? sum(remote) : sum(local);
    const result: TestResult = {
      udp: this.params.udp,
      reverse: this.params.reverse,
      durationSeconds: duration,
      sentBytes,
      receivedBytes,
      sendBitsPerSecond: bitsPerSecond(sentBytes, duration),
      receiveBitsPerSecond: bitsPerSecond(receivedBytes, duration),
      local,
      remote,
    };
    if (this.params.udp) {
      const receiverSide = sender ? remote : local;
      const packets = receiverSide.streams.reduce((a, s) => a + s.packets, 0);
      const lost = receiverSide.streams.reduce((a, s) => a + s.errors, 0);
      const jitterMs =
        (receiverSide.streams.reduce((a, s) => a + s.jitter, 0) /
          Math.max(1, receiverSide.streams.length)) * 1000;
      result.udpStats = {
        jitterMs, lost, packets,
        lostPercent: packets + lost > 0 ? (100 * lost) / (packets + lost) : 0,
      };
    }
    return result;
  }

  #cleanup(control: BytePipe): void {
    this.#running = false;
    if (this.#endTimer) clearTimeout(this.#endTimer);
    if (this.#intervalTimer) clearInterval(this.#intervalTimer);
    for (const s of this.#streams) s.close();
    control.close();
  }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
bun run test tests/client-iperf3.test.ts && bun run typecheck && bun run test
```
Expected: all PASS (including all earlier suites).

- [ ] **Step 6: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: client control state machine, verified against official iperf3"
```

---

### Task 8: Server — accept rule, single-test lock, verified against official iperf3 client

**Files:**
- Create: `packages/netsu/src/server.ts`
- Test: `packages/netsu/tests/server-iperf3.test.ts`
- Test: `packages/netsu/tests/ts-to-ts.test.ts`

**Interfaces:**
- Consumes: Tasks 3–7 (notably `StreamCounters`/`attachReceiver`/`startSender`, `TcpPipe`, codecs, states)
- Produces: `startServer(opts?: ServerOptions): Promise<NetsuServer>`, `interface ServerOptions { port?: number; transport?: "tcp" | "ws" }`, `interface NetsuServer { readonly port: number; close(): Promise<void> }`. Internal `ServerCore.handleConnection(pipe: BytePipe, toChannel: () => DataChannel): Promise<void>` — Task 10 reuses it for WS.

- [ ] **Step 1: Write the failing tests**

`packages/netsu/tests/server-iperf3.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { startServer } from "../src/server.ts";
import { HAS_IPERF3, nextPort, runIperf3Client } from "./helpers.ts";

interface Iperf3End {
  end: {
    sum_sent: { bytes: number };
    sum_received: { bytes: number };
  };
}

describe.skipIf(!HAS_IPERF3)("official iperf3 client vs netsu server (tcp)", () => {
  it("upload: iperf3 -c completes and reports bytes", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client(["-c", "127.0.0.1", "-p", String(port), "-t", "2"]);
      expect(code).toBe(0);
      const r = json as Iperf3End;
      expect(r.end.sum_sent.bytes).toBeGreaterThan(1_000_000);
      expect(r.end.sum_received.bytes).toBeGreaterThan(0);
    } finally {
      await server.close();
    }
  }, 15000);

  it("reverse: iperf3 -c -R receives from netsu server", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client(["-c", "127.0.0.1", "-p", String(port), "-t", "2", "-R"]);
      expect(code).toBe(0);
      expect((json as Iperf3End).end.sum_received.bytes).toBeGreaterThan(1_000_000);
    } finally {
      await server.close();
    }
  }, 15000);

  it("parallel: iperf3 -c -P 2", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const { code } = await runIperf3Client(["-c", "127.0.0.1", "-p", String(port), "-t", "2", "-P", "2"]);
      expect(code).toBe(0);
    } finally {
      await server.close();
    }
  }, 15000);
});
```

`packages/netsu/tests/ts-to-ts.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { nextPort } from "./helpers.ts";

describe("netsu client vs netsu server (tcp)", () => {
  for (const reverse of [false, true]) {
    for (const parallel of [1, 3]) {
      it(`reverse=${reverse} parallel=${parallel}`, async () => {
        const port = nextPort();
        const server = await startServer({ port });
        try {
          const r = await runClient("127.0.0.1", { port, duration: 1, reverse, parallel });
          expect(r.sentBytes).toBeGreaterThan(100_000);
          expect(r.receivedBytes).toBeGreaterThan(0);
          expect(r.receivedBytes).toBeLessThanOrEqual(r.sentBytes * 1.01);
          expect(r.local.streams).toHaveLength(parallel);
        } finally {
          await server.close();
        }
      }, 15000);
    }
  }

  it("serves a second test after the first finishes", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      await runClient("127.0.0.1", { port, duration: 1 });
      const again = await runClient("127.0.0.1", { port, duration: 1 });
      expect(again.sentBytes).toBeGreaterThan(0);
    } finally {
      await server.close();
    }
  }, 15000);

  it("rejects a concurrent client with ACCESS_DENIED", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const first = runClient("127.0.0.1", { port, duration: 2 });
      await new Promise((r) => setTimeout(r, 500));
      await expect(runClient("127.0.0.1", { port, duration: 1 })).rejects.toThrow(/busy/);
      await first;
    } finally {
      await server.close();
    }
  }, 15000);
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bun run test tests/server-iperf3.test.ts tests/ts-to-ts.test.ts
```
Expected: FAIL — `src/server.ts` not found.

- [ ] **Step 3: Implement the server**

`packages/netsu/src/server.ts`:

```ts
import { createServer } from "node:net";
import { bytesToCookie } from "./protocol/cookie.ts";
import { readJson, readState, writeJson, writeState } from "./protocol/framing.ts";
import type { BytePipe } from "./protocol/pipe.ts";
import { decodeParams, type TestParams } from "./protocol/params.ts";
import { decodeResults, encodeResults, type EndResults } from "./protocol/results.ts";
import {
  ACCESS_DENIED, COOKIE_SIZE, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS,
  PARAM_EXCHANGE, SERVER_ERROR, TEST_END, TEST_RUNNING, TEST_START,
} from "./protocol/states.ts";
import type { DataChannel } from "./streams/channel.ts";
import { attachReceiver, makeCounters, startSender, type StreamCounters } from "./streams/runner.ts";
import { TcpDataChannel, TcpPipe } from "./transport/tcp.ts";

export interface ServerOptions {
  port?: number;
  transport?: "tcp" | "ws";
}

export interface NetsuServer {
  readonly port: number;
  close(): Promise<void>;
}

const CONTROL_TIMEOUT = 30_000;

export async function startServer(opts: ServerOptions = {}): Promise<NetsuServer> {
  const port = opts.port ?? 5201;
  const transport = opts.transport ?? "tcp";
  const core = new ServerCore(port);
  if (transport !== "tcp") throw new Error("ws server wired in a later task"); // Task 10 replaces

  const server = createServer({ noDelay: true }, (socket) => {
    const pipe = new TcpPipe(socket);
    void core.handleConnection(pipe, () => new TcpDataChannel(pipe.detach()));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, () => {
      server.off("error", reject);
      resolve();
    });
  });
  return {
    port,
    close: () =>
      new Promise<void>((resolve) => {
        core.abort();
        server.close(() => resolve());
      }),
  };
}

/** Accept rule from iperf3's iperf_accept — shared by tcp (Task 8) and ws (Task 10). */
export class ServerCore {
  #active: ServerSession | null = null;

  constructor(readonly port: number) {}

  async handleConnection(pipe: BytePipe, toChannel: () => DataChannel): Promise<void> {
    try {
      const cookie = bytesToCookie(await pipe.readExact(COOKIE_SIZE, CONTROL_TIMEOUT));
      const active = this.#active;
      if (active?.wantsStream(cookie)) {
        active.addStream(toChannel());
        return;
      }
      if (active) {
        await writeState(pipe, ACCESS_DENIED);
        pipe.close();
        return;
      }
      const session = new ServerSession(cookie, pipe, this.port);
      this.#active = session;
      try {
        await session.run();
      } finally {
        this.#active = null;
      }
    } catch {
      pipe.close();
    }
  }

  abort(): void {
    this.#active?.abort();
  }
}

interface ServerStream {
  counters: StreamCounters;
  startSending(): void;
  finalize(): void;
  close(): void;
}

class ServerSession {
  #streams: ServerStream[] = [];
  #awaitingStreams = false;
  #streamArrived: (() => void) | undefined;
  #running = false;
  #startMs = 0;
  #endMs = 0;
  #params: TestParams | undefined;

  constructor(
    readonly cookie: string,
    private pipe: BytePipe,
    private port: number,
  ) {}

  wantsStream(cookie: string): boolean {
    return this.#awaitingStreams && cookie === this.cookie && this.#params?.udp !== true;
  }

  addStream(channel: DataChannel): void {
    const id = this.#streams.length + 1;
    const counters = makeCounters(id);
    const params = this.#params!;
    if (!params.reverse) attachReceiver(channel, counters);
    this.#streams.push({
      counters,
      startSending: () => {
        if (params.reverse) {
          void startSender(channel, counters, params.len, () => this.#running);
        }
      },
      finalize: () => {},
      close: () => channel.close(),
    });
    this.#streamArrived?.();
  }

  async run(): Promise<void> {
    const pipe = this.pipe;
    try {
      await writeState(pipe, PARAM_EXCHANGE);
      const params = decodeParams(await readJson(pipe, 65536, CONTROL_TIMEOUT));
      this.#params = params;
      if (params.udp) throw new Error("udp wired in a later task"); // Task 9 replaces this line

      this.#awaitingStreams = true;
      await writeState(pipe, CREATE_STREAMS);
      await this.#waitForStreams(params.parallel);
      this.#awaitingStreams = false;

      await writeState(pipe, TEST_START);
      this.#running = true;
      this.#startMs = Date.now();
      await writeState(pipe, TEST_RUNNING);
      for (const s of this.#streams) s.startSending();

      // Safety cap: client owns the timer; +10s grace (see PROTOCOL.md).
      const state = await readState(pipe, params.time * 1000 + 10_000);
      this.#running = false;
      this.#endMs = Date.now();
      if (state !== TEST_END) throw new Error(`expected TEST_END, got ${state}`);

      for (const s of this.#streams) s.finalize();
      await writeState(pipe, EXCHANGE_RESULTS);
      decodeResults(await readJson(pipe, 65536, CONTROL_TIMEOUT)); // client's view (kept implicit)
      await writeJson(pipe, encodeResults(this.#localResults()));
      await writeState(pipe, DISPLAY_RESULTS);
      await readState(pipe, CONTROL_TIMEOUT); // IPERF_DONE
    } catch {
      try {
        await writeState(pipe, SERVER_ERROR);
      } catch {
        // control channel already gone
      }
    } finally {
      this.#running = false;
      for (const s of this.#streams) s.close();
      pipe.close();
    }
  }

  #waitForStreams(n: number): Promise<void> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("timed out waiting for data streams")), CONTROL_TIMEOUT);
      const check = () => {
        if (this.#streams.length >= n) {
          clearTimeout(timer);
          this.#streamArrived = undefined;
          resolve();
        }
      };
      this.#streamArrived = check;
      check();
    });
  }

  #localResults(): EndResults {
    const params = this.#params!;
    const sender = params.reverse; // server sends when reversed
    const endSeconds = (this.#endMs - this.#startMs) / 1000;
    return {
      senderHasRetransmits: sender ? 0 : -1,
      streams: this.#streams.map(({ counters }) => ({
        id: counters.id,
        bytes: counters.bytes,
        retransmits: -1,
        jitter: counters.jitter,
        errors: counters.errors,
        packets: counters.packets,
        startTime: 0,
        endTime: endSeconds,
      })),
    };
  }

  abort(): void {
    this.#running = false;
    for (const s of this.#streams) s.close();
    this.pipe.close();
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
bun run test tests/server-iperf3.test.ts tests/ts-to-ts.test.ts && bun run typecheck && bun run test
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: server with iperf3 accept rule and single-test lock"
```

---

### Task 9: UDP — connect handshake, packet format, pacing, jitter/loss

**Files:**
- Create: `packages/netsu/src/transport/udp.ts`
- Modify: `packages/netsu/src/client.ts` (replace the `#openStream` udp throw with `#openUdpStream`)
- Modify: `packages/netsu/src/server.ts` (replace the udp throw with `#acceptUdpStreams`)
- Test: `packages/netsu/tests/udp-unit.test.ts`, `packages/netsu/tests/udp-interop.test.ts`

**Interfaces:**
- Consumes: `JitterTracker` (Task 5), `StreamCounters` (Task 7), client/server internals (Tasks 7–8)
- Produces (`transport/udp.ts`):
  - `UDP_CONNECT_MSG = 0x36373839`, `UDP_CONNECT_REPLY = 0x39383736`, `LEGACY_UDP_CONNECT_REPLY = 987654321`
  - `writeUdpHeader(buf: Buffer, pcount: number, nowMs: number): void` — `sec(u32BE) | usec(u32BE) | pcount(u32BE)` at offset 0
  - `readUdpHeader(buf: Buffer): { pcount: number; sentMs: number }`
  - `class Pacer { constructor(bitsPerSecond: number); gate(bits: number): Promise<void> }` — token bucket; `gate` resolves when the next send fits the rate
  - `udpClientConnect(host: string, port: number): Promise<dgram.Socket>` — connected socket, hello sent, reply received (accepts legacy), 5 s timeout
  - `udpServerBind(port: number): Promise<dgram.Socket>` — binds a fresh socket with `reuseAddr: true` on the shared port. **The first bind must happen BEFORE the server announces CREATE_STREAMS** — official iperf3 clients send their hello exactly once, so a late bind loses it (netsu server binds lazily per test, unlike iperf3 which binds at startup)
  - `udpServerAccept(socket: dgram.Socket, timeoutMs: number): Promise<dgram.Socket>` — waits for `UDP_CONNECT_MSG` on a bound socket, `connect()`s to the sender (kernel pins the 4-tuple), replies `UDP_CONNECT_REPLY`, returns the same socket. Caller loops bind→accept for each parallel stream — the iperf3 rebind trick, see PROTOCOL.md

- [ ] **Step 1: Write the failing unit tests**

`packages/netsu/tests/udp-unit.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { Pacer, readUdpHeader, writeUdpHeader } from "../src/transport/udp.ts";

describe("udp packet header", () => {
  it("round-trips pcount and timestamp", () => {
    const buf = Buffer.alloc(64);
    const now = 1_700_000_123_456;
    writeUdpHeader(buf, 42, now);
    const h = readUdpHeader(buf);
    expect(h.pcount).toBe(42);
    expect(Math.abs(h.sentMs - now)).toBeLessThan(1); // µs truncation only
  });
});

describe("Pacer", () => {
  it("holds ~1Mbit/s: 25 x 5000-bit sends take ≥ ~100ms", async () => {
    const pacer = new Pacer(1_000_000);
    const start = Date.now();
    for (let i = 0; i < 25; i++) await pacer.gate(5000);
    // 125_000 bits at 1Mbit/s = 125ms ideal; allow generous lower bound
    expect(Date.now() - start).toBeGreaterThanOrEqual(90);
  });

  it("does not throttle below the rate", async () => {
    const pacer = new Pacer(1_000_000_000);
    const start = Date.now();
    for (let i = 0; i < 25; i++) await pacer.gate(5000);
    expect(Date.now() - start).toBeLessThan(50);
  });
});
```

- [ ] **Step 2: Write the failing interop tests**

`packages/netsu/tests/udp-interop.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { HAS_IPERF3, nextPort, runIperf3Client, spawnIperf3Server } from "./helpers.ts";

describe.skipIf(!HAS_IPERF3)("udp vs official iperf3", () => {
  it("netsu client → iperf3 server: packets counted, loss small on loopback", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, udp: true, duration: 2, bandwidth: 5_000_000 });
      expect(r.udpStats).toBeDefined();
      expect(r.udpStats!.packets).toBeGreaterThan(100);
      expect(r.udpStats!.lostPercent).toBeLessThan(10);
      // rate should be near 5 Mbit/s, not unpaced
      expect(r.sendBitsPerSecond).toBeGreaterThan(2_000_000);
      expect(r.sendBitsPerSecond).toBeLessThan(10_000_000);
    } finally {
      kill();
    }
  }, 15000);

  it("iperf3 -u client → netsu server", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client([
        "-c", "127.0.0.1", "-p", String(port), "-t", "2", "-u", "-b", "5M",
      ]);
      expect(code).toBe(0);
      const end = (json as { end: { sum: { packets: number; lost_percent: number } } }).end;
      expect(end.sum.packets).toBeGreaterThan(100);
      expect(end.sum.lost_percent).toBeLessThan(10);
    } finally {
      await server.close();
    }
  }, 15000);
});

describe("udp netsu ↔ netsu", () => {
  for (const reverse of [false, true]) {
    it(`reverse=${reverse}`, async () => {
      const port = nextPort();
      const server = await startServer({ port });
      try {
        const r = await runClient("127.0.0.1", {
          port, udp: true, duration: 2, reverse, bandwidth: 5_000_000,
        });
        expect(r.udpStats!.packets).toBeGreaterThan(100);
        expect(r.udpStats!.lostPercent).toBeLessThan(10);
      } finally {
        await server.close();
      }
    }, 15000);
  }
});
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
bun run test tests/udp-unit.test.ts tests/udp-interop.test.ts
```
Expected: FAIL — `src/transport/udp.ts` not found.

- [ ] **Step 4: Implement transport/udp.ts**

```ts
import { createSocket, type RemoteInfo, type Socket } from "node:dgram";
import { setTimeout as delay } from "node:timers/promises";

// iperf.h stream-setup magic values
export const UDP_CONNECT_MSG = 0x36373839; // "6789"
export const UDP_CONNECT_REPLY = 0x39383736; // "9876"
export const LEGACY_UDP_CONNECT_REPLY = 987654321;

export const UDP_HEADER_SIZE = 12; // sec + usec + pcount, all u32 BE

export function writeUdpHeader(buf: Buffer, pcount: number, nowMs: number): void {
  const sec = Math.floor(nowMs / 1000);
  const usec = Math.floor((nowMs % 1000) * 1000);
  buf.writeUInt32BE(sec >>> 0, 0);
  buf.writeUInt32BE(usec >>> 0, 4);
  buf.writeUInt32BE(pcount >>> 0, 8);
}

export function readUdpHeader(buf: Buffer): { pcount: number; sentMs: number } {
  const sec = buf.readUInt32BE(0);
  const usec = buf.readUInt32BE(4);
  const pcount = buf.readUInt32BE(8);
  return { pcount, sentMs: sec * 1000 + usec / 1000 };
}

/** Token-bucket pacing: gate() delays so cumulative bits stay under rate. */
export class Pacer {
  #rate: number;
  #startMs = Date.now();
  #bitsSent = 0;

  constructor(bitsPerSecond: number) {
    this.#rate = bitsPerSecond;
  }

  async gate(bits: number): Promise<void> {
    this.#bitsSent += bits;
    if (this.#rate <= 0) return;
    const idealSec = this.#bitsSent / this.#rate;
    const aheadMs = idealSec * 1000 - (Date.now() - this.#startMs);
    if (aheadMs > 1) await delay(aheadMs);
  }
}

/** Client side of iperf3's UDP stream setup (iperf_udp_connect). */
export function udpClientConnect(host: string, port: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = createSocket("udp4");
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error("udp connect timeout"));
    }, 5000);
    socket.once("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
    socket.connect(port, host, () => {
      const onMsg = (msg: Buffer) => {
        const v = msg.length >= 4 ? msg.readUInt32BE(0) : -1;
        if (v === UDP_CONNECT_REPLY || v === LEGACY_UDP_CONNECT_REPLY) {
          clearTimeout(timer);
          socket.off("message", onMsg);
          resolve(socket);
        }
      };
      socket.on("message", onMsg);
      const hello = Buffer.alloc(4);
      hello.writeUInt32BE(UDP_CONNECT_MSG, 0);
      socket.send(hello);
    });
  });
}

/**
 * Bind a stream-accept socket on the shared UDP port. reuseAddr lets a fresh
 * socket bind while earlier (now connected) stream sockets keep the port —
 * the kernel routes each pinned 4-tuple to its connected socket.
 * The FIRST bind of a test must complete before CREATE_STREAMS is announced:
 * iperf3 clients send their hello exactly once.
 */
export function udpServerBind(port: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = createSocket({ type: "udp4", reuseAddr: true });
    socket.once("error", reject);
    socket.bind(port, () => {
      socket.off("error", reject);
      resolve(socket);
    });
  });
}

/**
 * Server side of iperf3's UDP stream setup (iperf_udp_accept): take the first
 * UDP_CONNECT_MSG on a bound socket, connect() to pin the peer, reply.
 */
export function udpServerAccept(socket: Socket, timeoutMs: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error("timed out waiting for udp stream"));
    }, timeoutMs);
    socket.once("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
    const onMsg = (msg: Buffer, rinfo: RemoteInfo) => {
      if (msg.length < 4 || msg.readUInt32BE(0) !== UDP_CONNECT_MSG) return;
      socket.off("message", onMsg);
      socket.connect(rinfo.port, rinfo.address, () => {
        const reply = Buffer.alloc(4);
        reply.writeUInt32BE(UDP_CONNECT_REPLY, 0);
        socket.send(reply);
        clearTimeout(timer);
        resolve(socket);
      });
    };
    socket.on("message", onMsg);
  });
}
```

- [ ] **Step 5: Wire UDP into the client**

In `packages/netsu/src/client.ts`, add imports:

```ts
import type { Socket } from "node:dgram";
import { JitterTracker } from "./stats.ts";
import {
  Pacer, readUdpHeader, UDP_HEADER_SIZE, udpClientConnect, writeUdpHeader,
} from "./transport/udp.ts";
import { randomBytes } from "node:crypto";
```

Replace the udp line in `#openStream` with `return this.#openUdpStream(id);` and add:

```ts
  async #openUdpStream(id: number): Promise<StreamHandle> {
    const socket = await udpClientConnect(this.host, this.port);
    const counters = makeCounters(id);
    if (this.params.reverse) {
      const tracker = new JitterTracker();
      socket.on("message", (msg: Buffer) => {
        if (msg.length < UDP_HEADER_SIZE) return;
        const { pcount, sentMs } = readUdpHeader(msg);
        tracker.onPacket(pcount, sentMs, Date.now());
        counters.bytes += msg.length;
        this.#meter.add(msg.length);
      });
      return {
        counters,
        start: () => {},
        finalize: () => {
          counters.packets = tracker.received;
          counters.errors = tracker.lost;
          counters.jitter = tracker.jitterMs / 1000;
        },
        close: () => socket.close(),
      };
    }
    return {
      counters,
      start: () => void this.#runUdpSender(socket, counters),
      finalize: () => {},
      close: () => socket.close(),
    };
  }

  async #runUdpSender(socket: Socket, counters: StreamCounters): Promise<void> {
    const len = this.params.len;
    const buf = randomBytes(len);
    const pacer = new Pacer(this.params.bandwidth);
    let pcount = 0;
    try {
      while (this.#running) {
        await pacer.gate(len * 8);
        if (!this.#running) break;
        writeUdpHeader(buf, ++pcount, Date.now());
        socket.send(buf);
        counters.bytes += len;
        counters.packets = pcount;
        this.#meter.add(len);
      }
    } catch {
      // socket closed at test end
    }
  }
```

- [ ] **Step 6: Wire UDP into the server**

In `packages/netsu/src/server.ts`, add the same udp imports (`Socket` type, `JitterTracker`, `Pacer`, `readUdpHeader`, `UDP_HEADER_SIZE`, `udpServerBind`, `udpServerAccept`, `writeUdpHeader`, `randomBytes`). Replace the udp throw plus the original `writeState(CREATE_STREAMS)` / `#waitForStreams` lines in `run()` with (there must be exactly one CREATE_STREAMS write, and for UDP the first bind must precede it — see udp.ts docs):

```ts
      this.#awaitingStreams = true;
      if (params.udp) {
        const first = await udpServerBind(this.port); // bound BEFORE the announce
        await writeState(pipe, CREATE_STREAMS);
        await this.#acceptUdpStreams(params, first);
      } else {
        await writeState(pipe, CREATE_STREAMS);
        await this.#waitForStreams(params.parallel);
      }
      this.#awaitingStreams = false;
```

and add the methods:

```ts
  async #acceptUdpStreams(params: TestParams, first: Socket): Promise<void> {
    let pending = first;
    for (let id = 1; id <= params.parallel; id++) {
      const socket = await udpServerAccept(pending, CONTROL_TIMEOUT);
      this.#streams.push(this.#makeUdpStream(id, socket, params));
      // Streams are opened sequentially by the client, so binding the next
      // accept socket after this stream's hello is race-free.
      if (id < params.parallel) pending = await udpServerBind(this.port);
    }
  }

  #makeUdpStream(id: number, socket: Socket, params: TestParams): ServerStream {
    const counters = makeCounters(id);
    if (!params.reverse) {
      const tracker = new JitterTracker();
      socket.on("message", (msg: Buffer) => {
        if (msg.length < UDP_HEADER_SIZE) return;
        const { pcount, sentMs } = readUdpHeader(msg);
        tracker.onPacket(pcount, sentMs, Date.now());
        counters.bytes += msg.length;
      });
      return {
        counters,
        startSending: () => {},
        finalize: () => {
          counters.packets = tracker.received;
          counters.errors = tracker.lost;
          counters.jitter = tracker.jitterMs / 1000;
        },
        close: () => socket.close(),
      };
    }
    return {
      counters,
      startSending: () => void this.#runUdpSender(socket, counters, params),
      finalize: () => {},
      close: () => socket.close(),
    };
  }

  async #runUdpSender(socket: Socket, counters: StreamCounters, params: TestParams): Promise<void> {
    const buf = randomBytes(params.len);
    const pacer = new Pacer(params.bandwidth);
    let pcount = 0;
    try {
      while (this.#running) {
        await pacer.gate(params.len * 8);
        if (!this.#running) break;
        writeUdpHeader(buf, ++pcount, Date.now());
        socket.send(buf);
        counters.bytes += params.len;
        counters.packets = pcount;
      }
    } catch {
      // closed at test end
    }
  }
```

Also update `wantsStream` — it already refuses when `#params.udp === true`, keep as is (UDP streams never arrive via TCP accept).

Note: UDP reverse needs a default bandwidth on the server side — `decodeParams` returns `bandwidth: 0` if the client omitted it; iperf3 clients always send `bandwidth` for UDP, and the netsu client defaults it, so a 0 here means "unpaced" and is acceptable only because both known clients always set it.

- [ ] **Step 7: Run tests to verify they pass**

```bash
bun run test tests/udp-unit.test.ts tests/udp-interop.test.ts && bun run typecheck && bun run test
```
Expected: all PASS.

- [ ] **Step 8: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: udp streams - connect handshake, pacing, jitter and loss stats"
```

---

### Task 10: WebSocket transport — same state machine over WS frames

**Files:**
- Create: `packages/netsu/src/transport/ws.ts`
- Modify: `packages/netsu/src/client.ts` (`#connectControl` + `#openStream` ws branches)
- Modify: `packages/netsu/src/server.ts` (`startServer` ws branch)
- Test: `packages/netsu/tests/ws.test.ts`

**Interfaces:**
- Consumes: `BytePipe`/`ByteBuffer` (Task 3), `DataChannel` (Task 6), `ServerCore` (Task 8)
- Produces (`transport/ws.ts`):
  - `class WsPipe implements BytePipe` with `detachToChannel(): WsDataChannel` (throws if bytes buffered)
  - `wsConnect(host: string, port: number): Promise<WsPipe>`
  - `class WsDataChannel implements DataChannel` — `write` throttles on `ws.bufferedAmount` (4 MiB high-water; this is the WS backpressure the old implementation lacked)

- [ ] **Step 1: Write the failing tests**

`packages/netsu/tests/ws.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { nextPort } from "./helpers.ts";

describe("netsu client vs netsu server (ws)", () => {
  for (const reverse of [false, true]) {
    for (const parallel of [1, 2]) {
      it(`reverse=${reverse} parallel=${parallel}`, async () => {
        const port = nextPort();
        const server = await startServer({ port, transport: "ws" });
        try {
          const r = await runClient("127.0.0.1", {
            port, transport: "ws", duration: 1, reverse, parallel,
          });
          expect(r.sentBytes).toBeGreaterThan(100_000);
          expect(r.receivedBytes).toBeGreaterThan(0);
          expect(r.receivedBytes).toBeLessThanOrEqual(r.sentBytes * 1.01);
          expect(r.local.streams).toHaveLength(parallel);
        } finally {
          await server.close();
        }
      }, 15000);
    }
  }

  it("ws server also enforces the single-test lock", async () => {
    const port = nextPort();
    const server = await startServer({ port, transport: "ws" });
    try {
      const first = runClient("127.0.0.1", { port, transport: "ws", duration: 2 });
      await new Promise((r) => setTimeout(r, 500));
      await expect(
        runClient("127.0.0.1", { port, transport: "ws", duration: 1 }),
      ).rejects.toThrow(/busy/);
      await first;
    } finally {
      await server.close();
    }
  }, 15000);
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bun run test tests/ws.test.ts
```
Expected: FAIL — "ws transport wired in a later task".

- [ ] **Step 3: Implement transport/ws.ts**

```ts
import { setTimeout as delay } from "node:timers/promises";
import { WebSocket, type RawData } from "ws";
import { ByteBuffer, type BytePipe } from "../protocol/pipe.ts";
import type { DataChannel } from "../streams/channel.ts";

function toBuffer(data: RawData): Buffer {
  if (Buffer.isBuffer(data)) return data;
  if (Array.isArray(data)) return Buffer.concat(data);
  return Buffer.from(data);
}

/** WS binary frames as a byte pipe — identical byte sequence to TCP (PROTOCOL.md). */
export class WsPipe implements BytePipe {
  readonly ws: WebSocket;
  #buffer = new ByteBuffer();
  #onMessage = (d: RawData) => this.#buffer.feed(toBuffer(d));
  #onClose = () => this.#buffer.end();

  constructor(ws: WebSocket) {
    this.ws = ws;
    ws.on("message", this.#onMessage);
    ws.on("close", this.#onClose);
    ws.on("error", this.#onClose);
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    return this.#buffer.readExact(n, timeoutMs);
  }

  write(data: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      this.ws.send(data, (err) => (err ? reject(err) : resolve()));
    });
  }

  /** Switch this connection from control framing to bulk payload. */
  detachToChannel(): WsDataChannel {
    if (this.#buffer.buffered > 0) throw new Error("detach with buffered bytes");
    this.ws.off("message", this.#onMessage);
    this.ws.off("close", this.#onClose);
    this.ws.off("error", this.#onClose);
    return new WsDataChannel(this.ws);
  }

  close(): void {
    this.ws.terminate();
    this.#buffer.end();
  }
}

export function wsConnect(host: string, port: number): Promise<WsPipe> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`ws://${host}:${port}/`);
    ws.once("open", () => {
      ws.off("error", reject);
      resolve(new WsPipe(ws));
    });
    ws.once("error", reject);
  });
}

const HIGH_WATER = 4 * 1024 * 1024;

/** Bulk payload over WS. bufferedAmount polling is the backpressure gate. */
export class WsDataChannel implements DataChannel {
  #ws: WebSocket;

  constructor(ws: WebSocket) {
    this.#ws = ws;
    ws.on("error", () => ws.terminate());
  }

  async write(chunk: Uint8Array): Promise<void> {
    while (this.#ws.bufferedAmount > HIGH_WATER) {
      if (this.#ws.readyState !== WebSocket.OPEN) throw new Error("ws closed");
      await delay(2);
    }
    if (this.#ws.readyState !== WebSocket.OPEN) throw new Error("ws closed");
    return new Promise((resolve, reject) => {
      this.#ws.send(chunk, (err) => (err ? reject(err) : resolve()));
    });
  }

  onData(cb: (byteLength: number) => void): void {
    this.#ws.on("message", (d: RawData) => cb(toBuffer(d).length));
  }

  close(): void {
    this.#ws.terminate();
  }
}
```

- [ ] **Step 4: Wire WS into the client**

In `packages/netsu/src/client.ts`, add import `import { wsConnect } from "./transport/ws.ts";` and replace `#connectControl` and the ws line of `#openStream`:

```ts
  #connectControl(): Promise<BytePipe> {
    return this.transport === "ws"
      ? wsConnect(this.host, this.port)
      : tcpConnect(this.host, this.port);
  }

  async #openStream(id: number): Promise<StreamHandle> {
    if (this.params.udp) return this.#openUdpStream(id);
    if (this.transport === "ws") return this.#openWsStream(id);
    return this.#openTcpStream(id);
  }

  async #openWsStream(id: number): Promise<StreamHandle> {
    const pipe = await wsConnect(this.host, this.port);
    await pipe.write(cookieToBytes(this.cookie));
    const channel = pipe.detachToChannel();
    const counters = makeCounters(id);
    if (this.params.reverse) {
      attachReceiver(channel, counters, (n) => this.#meter.add(n));
    }
    return {
      counters,
      start: () => {
        if (!this.params.reverse) {
          void startSender(channel, counters, this.params.len, () => this.#running, (n) =>
            this.#meter.add(n),
          );
        }
      },
      finalize: () => {},
      close: () => channel.close(),
    };
  }
```

- [ ] **Step 5: Wire WS into the server**

In `packages/netsu/src/server.ts`, add imports `import { WebSocketServer, type WebSocket } from "ws";` and `import { WsPipe } from "./transport/ws.ts";`. Replace the whole `startServer` function:

```ts
export async function startServer(opts: ServerOptions = {}): Promise<NetsuServer> {
  const port = opts.port ?? 5201;
  const transport = opts.transport ?? "tcp";
  const core = new ServerCore(port);

  if (transport === "ws") {
    const wss = new WebSocketServer({ port });
    const sockets = new Set<WebSocket>();
    wss.on("connection", (ws) => {
      sockets.add(ws);
      ws.on("close", () => sockets.delete(ws));
      const pipe = new WsPipe(ws);
      void core.handleConnection(pipe, () => pipe.detachToChannel());
    });
    await new Promise<void>((resolve, reject) => {
      wss.once("listening", resolve);
      wss.once("error", reject);
    });
    return {
      port,
      close: () =>
        new Promise<void>((resolve) => {
          core.abort();
          for (const ws of sockets) ws.terminate();
          wss.close(() => resolve());
        }),
    };
  }

  const server = createServer({ noDelay: true }, (socket) => {
    const pipe = new TcpPipe(socket);
    void core.handleConnection(pipe, () => new TcpDataChannel(pipe.detach()));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, () => {
      server.off("error", reject);
      resolve();
    });
  });
  return {
    port,
    close: () =>
      new Promise<void>((resolve) => {
        core.abort();
        server.close(() => resolve());
      }),
  };
}
```

- [ ] **Step 6: Run tests to verify they pass**

```bash
bun run test tests/ws.test.ts && bun run typecheck && bun run test
```
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: websocket transport - protocol tunneled over ws frames"
```

---

### Task 11: CLI — iperf3-style flags, interval lines, --json

**Files:**
- Create: `packages/netsu/src/format.ts`
- Modify: `packages/netsu/src/cli.ts` (replace placeholder)
- Test: `packages/netsu/tests/format.test.ts`, `packages/netsu/tests/cli.test.ts`

**Interfaces:**
- Consumes: `runClient`/`startServer`/`TestResult`/`IntervalReport`
- Produces (`format.ts`): `parseBandwidth(s: string): number` ("5M" → 5_000_000 bits/s; K/M/G = 1e3/1e6/1e9), `formatBytes(n: number): string` ("112 MBytes", 1024-based), `formatBits(n: number): string` ("943 Mbits/sec", 1000-based), `intervalLine(r: IntervalReport): string`

- [ ] **Step 1: Write the failing format tests**

`packages/netsu/tests/format.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { formatBits, formatBytes, intervalLine, parseBandwidth } from "../src/format.ts";

describe("parseBandwidth", () => {
  it("parses plain numbers and K/M/G suffixes (bits/s)", () => {
    expect(parseBandwidth("1000")).toBe(1000);
    expect(parseBandwidth("5M")).toBe(5_000_000);
    expect(parseBandwidth("2.5m")).toBe(2_500_000);
    expect(parseBandwidth("1G")).toBe(1_000_000_000);
    expect(() => parseBandwidth("fast")).toThrow();
  });
});

describe("formatters", () => {
  it("formats byte and bit magnitudes", () => {
    expect(formatBytes(117_440_512)).toBe("112 MBytes");
    expect(formatBytes(512)).toBe("512 Bytes");
    expect(formatBits(943_000_000)).toBe("943 Mbits/sec");
    expect(formatBits(1_500)).toBe("1.50 Kbits/sec");
  });

  it("renders an interval line", () => {
    const line = intervalLine({ start: 0, end: 1, bytes: 117_440_512, bitsPerSecond: 943_000_000 });
    expect(line).toContain("0.00-1.00");
    expect(line).toContain("112 MBytes");
    expect(line).toContain("943 Mbits/sec");
  });
});
```

- [ ] **Step 2: Write the failing CLI smoke test**

`packages/netsu/tests/cli.test.ts`:

```ts
import { spawn, type ChildProcess } from "node:child_process";
import { afterEach, describe, expect, it } from "vitest";
import { nextPort } from "./helpers.ts";

const procs: ChildProcess[] = [];
afterEach(() => {
  while (procs.length) procs.pop()!.kill("SIGKILL");
});

function run(args: string[]): ChildProcess {
  const proc = spawn("bun", ["src/cli.ts", ...args], { stdio: ["ignore", "pipe", "pipe"] });
  procs.push(proc);
  return proc;
}

function waitForOutput(proc: ChildProcess, needle: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`no "${needle}" in output`)), 8000);
    proc.stdout!.on("data", (d: Buffer) => {
      if (d.toString().includes(needle)) {
        clearTimeout(timer);
        resolve();
      }
    });
  });
}

function collect(proc: ChildProcess): Promise<{ code: number; out: string }> {
  let out = "";
  proc.stdout!.on("data", (d: Buffer) => (out += d.toString()));
  return new Promise((resolve) => proc.on("close", (code) => resolve({ code: code ?? -1, out })));
}

describe("cli", () => {
  it("server + client run a tcp test end to end", async () => {
    const port = nextPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");
    const { code, out } = await collect(run(["client", "127.0.0.1", "-p", String(port), "-t", "1"]));
    expect(code).toBe(0);
    expect(out).toContain("sender");
    expect(out).toContain("receiver");
  }, 20000);

  it("--json emits parseable iperf3-shaped output", async () => {
    const port = nextPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");
    const { code, out } = await collect(
      run(["client", "127.0.0.1", "-p", String(port), "-t", "1", "--json"]),
    );
    expect(code).toBe(0);
    const parsed = JSON.parse(out) as {
      end: { sum_sent: { bytes: number }; sum_received: { bytes: number } };
    };
    expect(parsed.end.sum_sent.bytes).toBeGreaterThan(0);
    expect(parsed.end.sum_received.bytes).toBeGreaterThan(0);
  }, 20000);
});
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
bun run test tests/format.test.ts tests/cli.test.ts
```
Expected: FAIL — format.ts missing; cli is a placeholder.

- [ ] **Step 4: Implement format.ts and cli.ts**

`packages/netsu/src/format.ts`:

```ts
import type { IntervalReport } from "./stats.ts";

/** "5M" → 5_000_000 bits/s. K/M/G are decimal, like iperf3's -b. */
export function parseBandwidth(s: string): number {
  const m = /^(\d+(?:\.\d+)?)([kKmMgG])?$/.exec(s);
  if (!m) throw new Error(`invalid bandwidth: ${s}`);
  const mult = { k: 1e3, m: 1e6, g: 1e9 }[(m[2] ?? "").toLowerCase()] ?? 1;
  return Math.round(Number(m[1]) * mult);
}

export function formatBytes(n: number): string {
  let value = n;
  const units = ["Bytes", "KBytes", "MBytes", "GBytes", "TBytes"];
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i++;
  }
  const text = value >= 100 || Number.isInteger(value) ? String(Math.round(value)) : value.toFixed(2);
  return `${text} ${units[i]}`;
}

export function formatBits(n: number): string {
  let value = n;
  const units = ["bits/sec", "Kbits/sec", "Mbits/sec", "Gbits/sec"];
  let i = 0;
  while (value >= 1000 && i < units.length - 1) {
    value /= 1000;
    i++;
  }
  const text = value >= 100 || Number.isInteger(value) ? String(Math.round(value)) : value.toFixed(2);
  return `${text} ${units[i]}`;
}

export function intervalLine(r: IntervalReport): string {
  const range = `${r.start.toFixed(2)}-${r.end.toFixed(2)}`.padStart(11);
  return `[SUM] ${range} sec  ${formatBytes(r.bytes).padStart(12)}  ${formatBits(r.bitsPerSecond).padStart(14)}`;
}
```

`packages/netsu/src/cli.ts`:

```ts
#!/usr/bin/env node
import { defineCommand, runMain } from "citty";
import { runClient, type TestResult } from "./client.ts";
import { formatBits, formatBytes, intervalLine, parseBandwidth } from "./format.ts";
import { startServer } from "./server.ts";
import type { IntervalReport } from "./stats.ts";

const serverCmd = defineCommand({
  meta: { name: "server", description: "Start a netsu speed test server" },
  args: {
    port: { type: "string", alias: "p", default: "5201", description: "listen port" },
    ws: { type: "boolean", default: false, description: "WebSocket mode (netsu-only)" },
  },
  async run({ args }) {
    const port = Number.parseInt(args.port, 10);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new Error(`invalid port: ${args.port}`);
    }
    await startServer({ port, transport: args.ws ? "ws" : "tcp" });
    console.log(`netsu server listening on ${port} (${args.ws ? "ws" : "tcp"})`);
    // server keeps the event loop alive; Ctrl-C to stop
  },
});

const clientCmd = defineCommand({
  meta: { name: "client", description: "Run a speed test against a server" },
  args: {
    host: { type: "positional", required: true, description: "server host" },
    port: { type: "string", alias: "p", default: "5201" },
    time: { type: "string", alias: "t", default: "10", description: "duration seconds" },
    udp: { type: "boolean", alias: "u", default: false },
    ws: { type: "boolean", default: false },
    parallel: { type: "string", alias: "P", default: "1" },
    reverse: { type: "boolean", alias: "R", default: false },
    bandwidth: { type: "string", alias: "b", description: "UDP pacing, e.g. 5M (bits/s)" },
    len: { type: "string", alias: "l", description: "block size bytes" },
    interval: { type: "string", alias: "i", default: "1" },
    json: { type: "boolean", default: false },
  },
  async run({ args }) {
    const num = (s: string, name: string): number => {
      const v = Number.parseInt(s, 10);
      if (!Number.isInteger(v) || v <= 0) throw new Error(`invalid ${name}: ${s}`);
      return v;
    };
    if (args.udp && args.ws) throw new Error("--udp and --ws are mutually exclusive");
    const intervals: IntervalReport[] = [];
    const intervalSec = num(args.interval, "interval");
    const result = await runClient(args.host, {
      port: num(args.port, "port"),
      duration: num(args.time, "time"),
      parallel: num(args.parallel, "parallel"),
      udp: args.udp,
      transport: args.ws ? "ws" : "tcp",
      reverse: args.reverse,
      bandwidth: args.bandwidth ? parseBandwidth(args.bandwidth) : undefined,
      len: args.len ? num(args.len, "len") : undefined,
      interval: intervalSec,
      onInterval: (r) => {
        intervals.push(r);
        if (!args.json) console.log(intervalLine(r));
      },
    });
    if (args.json) console.log(JSON.stringify(toJson(result, intervals), null, 2));
    else printSummary(result);
  },
});

function printSummary(r: TestResult): void {
  const dur = r.durationSeconds.toFixed(2);
  console.log("- - - - - - - - - - - - - - - - - - - - - - - - -");
  console.log(
    `[SUM]  0.00-${dur} sec  ${formatBytes(r.sentBytes).padStart(12)}  ${formatBits(r.sendBitsPerSecond).padStart(14)}  sender`,
  );
  console.log(
    `[SUM]  0.00-${dur} sec  ${formatBytes(r.receivedBytes).padStart(12)}  ${formatBits(r.receiveBitsPerSecond).padStart(14)}  receiver`,
  );
  if (r.udpStats) {
    const u = r.udpStats;
    console.log(
      `[SUM] jitter ${u.jitterMs.toFixed(3)} ms, lost ${u.lost}/${u.packets + u.lost} (${u.lostPercent.toFixed(2)}%)`,
    );
  }
}

function toJson(r: TestResult, intervals: IntervalReport[]): Record<string, unknown> {
  return {
    start: {
      version: "netsu-0.2.0",
      test_start: { protocol: r.udp ? "UDP" : "TCP", reverse: r.reverse ? 1 : 0 },
    },
    intervals: intervals.map((i) => ({
      sum: { start: i.start, end: i.end, bytes: i.bytes, bits_per_second: i.bitsPerSecond },
    })),
    end: {
      sum_sent: { bytes: r.sentBytes, bits_per_second: r.sendBitsPerSecond },
      sum_received: { bytes: r.receivedBytes, bits_per_second: r.receiveBitsPerSecond },
      ...(r.udpStats
        ? {
            sum: {
              jitter_ms: r.udpStats.jitterMs,
              lost_packets: r.udpStats.lost,
              packets: r.udpStats.packets,
              lost_percent: r.udpStats.lostPercent,
            },
          }
        : {}),
    },
  };
}

await runMain(
  defineCommand({
    meta: { name: "netsu", description: "iperf3-compatible network speed test" },
    subCommands: { server: serverCmd, client: clientCmd },
  }),
);
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
bun run test tests/format.test.ts tests/cli.test.ts && bun run typecheck && bun run test
```
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: cli with iperf3-style flags, interval lines and --json"
```

---

### Task 12: Public API, README, packaging, CI gates

**Files:**
- Modify: `packages/netsu/src/index.ts`
- Create: `packages/netsu/jsr.json`
- Modify: `packages/netsu/README.md`, root `README.md`
- Modify: `.github/workflows/` (both existing workflow files — check exact names with `ls .github/workflows/`)

**Interfaces:**
- Consumes: everything
- Produces: the published API surface: `runClient`, `startServer`, `VERSION`, and the option/result types

- [ ] **Step 1: Finalize index.ts**

```ts
export { runClient } from "./client.ts";
export type { ClientOptions, TestResult, UdpStats } from "./client.ts";
export { startServer } from "./server.ts";
export type { NetsuServer, ServerOptions } from "./server.ts";
export type { IntervalReport } from "./stats.ts";
export type { EndResults, StreamResult } from "./protocol/results.ts";
export const VERSION = "0.2.0";
```

- [ ] **Step 2: jsr.json**

`packages/netsu/jsr.json`:

```json
{
  "name": "@hk/netsu",
  "version": "0.2.0",
  "license": "MIT",
  "exports": "./src/index.ts"
}
```

- [ ] **Step 3: Rewrite package README**

`packages/netsu/README.md`:

````markdown
# netsu

iperf3-compatible network speed test — TypeScript library and CLI.

- Speaks the **iperf3 wire protocol**: test against an official `iperf3 -s`,
  or point `iperf3 -c` at a netsu server.
- Adds a **WebSocket mode** (netsu ↔ netsu only) that traverses HTTP proxies.
- TCP / UDP / WS × upload / reverse, parallel streams, interval reports,
  UDP jitter & loss, `--json` output.

## CLI

```bash
# server
npx netsu server -p 5201            # tcp+udp, iperf3-compatible
npx netsu server -p 5201 --ws       # websocket mode

# client
npx netsu client <host> -t 5                 # tcp upload
npx netsu client <host> -R                   # reverse: server sends
npx netsu client <host> -u -b 10M            # udp at 10 Mbit/s
npx netsu client <host> --ws -P 4            # websocket, 4 streams
npx netsu client <host> --json               # machine-readable output
```

Works against official iperf3 either way:

```bash
iperf3 -s -p 5201            # official server …
npx netsu client localhost   # … netsu client

npx netsu server -p 5201     # netsu server …
iperf3 -c localhost          # … official client
```

## Library

```ts
import { runClient, startServer } from "netsu";

const server = await startServer({ port: 5201 });

const result = await runClient("127.0.0.1", {
  port: 5201,
  duration: 5,
  reverse: false,
  parallel: 2,
  onInterval: (r) => console.log(r.bitsPerSecond),
});
console.log(result.sendBitsPerSecond, result.receiveBitsPerSecond);

await server.close();
```

The wire protocol is documented in [PROTOCOL.md](../../PROTOCOL.md).
````

Update root `README.md` to state the repo layout (ts package, rust crate coming in phase 2, PROTOCOL.md) in a few lines.

- [ ] **Step 4: Fix CI workflows**

`ls .github/workflows/` to get the two file names. Replace the CI workflow content with:

```yaml
name: CI
on:
  push:
    branches: [main, develop]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: oven-sh/setup-bun@v2
      - run: sudo apt-get update && sudo apt-get install -y iperf3
      - run: bun install
      - run: bun run typecheck
        working-directory: packages/netsu
      - run: bun run test
        working-directory: packages/netsu
      - run: bun run build
        working-directory: packages/netsu
```

Replace the publish workflow content with (publish only from main, gated on the same checks — the old workflow published broken code from develop):

```yaml
name: Publish
on:
  push:
    branches: [main]

jobs:
  publish:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v4
      - uses: oven-sh/setup-bun@v2
      - run: sudo apt-get update && sudo apt-get install -y iperf3
      - run: bun install
      - run: bun run typecheck
        working-directory: packages/netsu
      - run: bun run test
        working-directory: packages/netsu
      - run: bun run build
        working-directory: packages/netsu
      - run: bunx jsr publish --allow-slow-types
        working-directory: packages/netsu
```

- [ ] **Step 5: Full verification**

```bash
cd /Users/hk/Dev/netsu/packages/netsu
bun run typecheck && bun run test && bun run build
npm pack --dry-run 2>&1 | grep -E "dist/"
```
Expected: everything passes; the pack listing contains `dist/index.mjs`, `dist/cli.mjs`, `dist/index.d.mts`.

- [ ] **Step 6: Commit**

```bash
cd /Users/hk/Dev/netsu
git add -A && git commit -m "feat: public api, readme, jsr config, ci gates"
```

---

## Plan Self-Review Notes (kept for the executor)

- Spec coverage: repo demolition (T1), PROTOCOL.md (T2), protocol core (T3–T4), stats (T5), transports tcp/udp/ws (T6/T9/T10), client (T7), server + single-test lock + ACCESS_DENIED (T8), UDP pacing/jitter/loss + reflection-safety via connected sockets (T9), WS backpressure (T10), intervals/`--json`/CLI (T11), packaging + CI publish gate (T12). Docker e2e interop matrix and Rust are Phases 2–3 by design.
- Known simplifications vs iperf3, all deliberate: `omit`/`-n`/`-k`/`--bidir` unsupported (rejected implicitly — netsu never sends them; a client sending them gets them ignored); retransmit counts always -1 (no TCP_INFO in Node); cpu_util reported as 0; IPv4 only for UDP sockets (`udp4`).
- If official-iperf3 interop tests fail on a field mismatch, the referee is right: fix netsu against the cloned iperf3 source, then update PROTOCOL.md in the same commit.

