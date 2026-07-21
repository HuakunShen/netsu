# RendezKey Cloudflare WebRTC Signaling Implementation Plan

> **Repository:** `/Users/hk/Dev/netsu` > **Application:** `apps/rendez-key` > **Consumer:** `netsu-rs` and the browser/container interoperability fixture
> **Scope:** signaling implementation only; the RendezKey migration into this
> repository is tracked separately and must already be complete.

**Goal:** Add a public, short-lived WebRTC signaling service to the existing
RendezKey Cloudflare Worker without changing the semantics of its D1-backed
entry store/claim API.

**Decision:** Merge at the Worker deployment and shared infrastructure layer,
not at the persistence model. Existing `/v1/entries` routes continue using D1.
New `/v1/signal` routes use one Durable Object per room and Cloudflare WebSocket
Hibernation. There is no Bun server implementation and no TURN relay.

**Why this shape:** RendezKey already owns the custom domain, short-code
alphabet/normalization, Hono/OpenAPI conventions, rate limiting, observability,
Wrangler config, and Cloudflare Vitest harness. A signaling room, however, is a
two-party concurrent state machine, not an immutable value waiting for atomic
claim. Durable Objects provide the correct single-room concurrency boundary.

**Current stack to retain:** Hono `4.12.31`, Wrangler `4.112.0`, TypeScript
`5.9.2`, Vitest `4.1.10`, `@cloudflare/vitest-pool-workers`, Zod, existing D1
and Drizzle routes.

## 0. Non-negotiable boundaries

- Do not change `POST /v1/entries` or `POST /v1/entries/:code/claim` behavior.
- Do not store active rooms, SDP, or ICE candidates in D1.
- Do not store active rooms in module/global Worker memory.
- Do not add a Bun `Bun.serve` or Hono `hono/bun` adapter.
- Run the Worker locally with `wrangler dev`; run tests in Cloudflare's pool.
- Use one `SIGNAL_ROOMS.getByName(normalizedCode)` Durable Object per room.
- Use WebSocket Hibernation (`ctx.acceptWebSocket`) rather than
  `server.accept()`/event listeners.
- Do not log SDP, candidate strings, IP addresses, or listener secrets.
- Do not proxy benchmark DataChannel payload through the Worker.
- Do not add TURN credential issuance or relay endpoints.
- Add all Hono routes with the repository's chained typing style and attach
  request/response validation plus OpenAPI metadata.
- Regenerate Worker binding types after Wrangler configuration changes.

## 1. Route and state contract

### HTTP create

```http
POST /v1/signal/rooms
Content-Type: application/json

{"v":1,"ttl_seconds":600}
```

Success `201`:

```json
{
  "v": 1,
  "code": "ABCD-EFGH",
  "listener_secret": "<256-bit base64url>",
  "expires_at": "2026-07-22T12:34:56.000Z"
}
```

The endpoint generates a human-safe eight-character code using the existing
alphabet and normalization functions. It generates a 256-bit listener secret,
stores only its SHA-256 hash in the room object, and returns the clear secret
once. A name collision retries up to five times, then returns `503`.

### WebSocket upgrade

```http
GET /v1/signal/rooms/:code/ws
Upgrade: websocket
```

The Hono route validates/normalizes `code`, routes to
`env.SIGNAL_ROOMS.getByName(code)`, and delegates the upgrade to that object.
The first client message, due within five seconds, binds the socket:

```json
{"v":1,"type":"bind","role":"listener","secret":"..."}
{"v":1,"type":"bind","role":"joiner"}
```

The secret is deliberately not in a URL, query parameter, cookie, or
subprotocol header, which reduces accidental access-log exposure. A room code
is the joiner's short-lived bearer capability.

### Room state

```text
empty/uninitialized
  -> listener-created
  -> listener-bound
  -> paired
  -> closed/expired (terminal, never reusable)
```

Per-room hard limits:

- TTL default 600s, accepted 60..3600s;
- two bound sockets and at most one socket awaiting bind per role;
- first message/bind deadline 5s;
- WebSocket text only, 64 KiB per frame;
- 16 candidate messages per peer;
- 1 MiB total forwarded signaling bytes;
- one offer and one answer;
- no retained SDP/candidate payload after forwarding;
- any policy violation sends a bounded error then closes with 1008;
- expiry closes with 1001; internal failure uses 1011.

Persisted room metadata is limited to version, lifecycle state, created/expiry
timestamps, listener-secret hash, candidate/byte counters, and terminal reason.
Socket role/session metadata is serialized into WebSocket attachments so it
survives hibernation.

## 2. Planned netsu file map

Create:

- `apps/rendez-key/src/signal/protocol.ts`
- `apps/rendez-key/src/signal/limits.ts`
- `apps/rendez-key/src/signal/secret.ts`
- `apps/rendez-key/src/signal/room.ts`
- `apps/rendez-key/src/routes/create-signal-room.ts`
- `apps/rendez-key/src/routes/connect-signal-room.ts`
- `apps/rendez-key/test/signal-protocol.test.ts`
- `apps/rendez-key/test/signal-room.test.ts`
- `apps/rendez-key/test/signal-routes.test.ts`
- `apps/rendez-key/test/fixtures/signal-v1.json`
- `apps/rendez-key/scripts/signal-smoke-test.mjs`

Modify:

- `apps/rendez-key/src/app.ts`
- `apps/rendez-key/src/index.ts`
- `apps/rendez-key/src/domain/code.ts` only if a reusable generator export is
  missing; do not change existing code behavior.
- `apps/rendez-key/src/domain/limits.ts` only for shared constants; keep entry
  and signal limits structurally separate.
- `apps/rendez-key/src/openapi/schemas.ts`
- `apps/rendez-key/wrangler.jsonc`
- `apps/rendez-key/worker-configuration.d.ts` via `wrangler types`
- `apps/rendez-key/test/env.d.ts`
- `apps/rendez-key/vitest.config.ts` only if DO test bindings require it.
- `apps/rendez-key/package.json`
- `apps/rendez-key/README.md`
- `apps/rendez-key/scripts/smoke-test.sh`
- relevant netsu CI workflow discovered at execution time.

## Task 1: Freeze signaling v1 schemas and resource limits

1. Add failing tests for every valid message and these invalid cases: wrong
   version/type, missing/extra fields, invalid role, non-offer/non-answer SDP
   type, candidate string too long, invalid indices, binary payload marker,
   over-64-KiB frame, and listener secret wrong length/encoding.
2. Define Zod schemas and inferred types in `signal/protocol.ts`. Validation must
   parse untrusted input; no unchecked cast from JSON.
3. Define separate signal limits. Do not reuse D1 body/read-count values merely
   because their numbers happen to match.
4. Commit a golden fixture corpus consumed later by netsu Rust and Chromium
   tests. It includes successes and expected error codes but no real SDP/IP.
5. Define stable public errors:
   `invalid_message`, `room_not_found`, `room_expired`, `room_full`,
   `unauthorized_listener`, `unexpected_message`, `resource_limit`,
   `internal_error`.

**Verify:**

```bash
cd /Users/hk/Dev/netsu
bun run --cwd apps/rendez-key test -- signal-protocol.test.ts
bun run signal:typecheck
```

**Commit boundary:** `feat(rendez-key): define signaling v1 protocol`

## Task 2: Implement secret handling and room initialization

1. Add failing tests for 256-bit randomness shape, base64url encoding, hashing,
   equal/wrong hash comparison, and redacted serialization/debug output.
2. Generate secrets with Web Crypto. Hash with `crypto.subtle.digest('SHA-256',
...)`. Compare equal-length digest bytes with a full XOR loop.
3. Implement `SignalRoom extends DurableObject<CloudflareBindings>` using the
   current `cloudflare:workers` base class.
4. Add an RPC method or private authenticated internal fetch that initializes
   a room exactly once. Concurrent/repeated initialization returns conflict and
   never overwrites the first secret/expiry.
5. Persist metadata before returning create success. Schedule an alarm at
   `expiresAt`. A retry after uncertain failure must observe initialized state.
6. Terminal rooms stay terminal until storage cleanup; the same code cannot be
   recreated during its original expiry window.

**Verify:** run the room unit tests five times and trigger alarms with
`runDurableObjectAlarm`.

**Commit boundary:** `feat(rendez-key): add durable signaling room state`

## Task 3: Implement WebSocket Hibernation and forwarding state machine

1. Add failing tests for upgrade validation, five-second bind deadline, correct
   listener secret, wrong listener secret, one joiner, second joiner, ordered
   forwarding, invalid role/state, candidate/frame/total-byte limits, peer-left,
   close/error idempotence, and alarm expiry.
2. In the DO `fetch`, require `GET` plus `Upgrade: websocket`, create a
   `WebSocketPair`, and call `ctx.acceptWebSocket(server, tags)`.
3. Implement `webSocketMessage`, `webSocketClose`, and `webSocketError`; do not
   attach event listeners or rely on an in-memory socket map as authority.
4. Read/write role and bound/session metadata through WebSocket attachments.
   Recover active sockets with `ctx.getWebSockets()` after hibernation.
5. Validate before forwarding. Forward to the opposite bound role only; never
   echo SDP/candidates to the sender and never retain their bodies.
6. Make cleanup idempotent across leave, close, error, alarm, and unexpected
   exceptions. Notify the other peer with `peer_left` when possible.
7. Add structured counters only: room_created, room_paired, room_expired,
   protocol_error_code, duration bucket. Never log room code at info level.

**Verify:**

```bash
bun run --cwd apps/rendez-key test -- signal-room.test.ts
for i in 1 2 3 4 5; do
  bun run --cwd apps/rendez-key test -- signal-room.test.ts || exit 1
done
```

**Commit boundary:** `feat(rendez-key): forward signaling with WebSocket hibernation`

## Task 4: Add Hono creation and upgrade routes

1. Add failing Worker-pool tests through `SELF.fetch`, not direct function-only
   calls: auth/open-mode policy, rate limit, create `201`, malformed TTL `400`,
   disabled anonymous create `403`, collision retry, normalized WS code, missing
   room `404`, non-upgrade `426`, and real two-socket offer/answer flow.
2. Add `POST /v1/signal/rooms` with Zod validation and complete OpenAPI metadata.
3. Add `GET /v1/signal/rooms/:code/ws`; document the `101` protocol contract in
   OpenAPI/README even if an interactive OpenAPI client cannot hold the socket.
4. Keep Hono route definitions chained so binding/path inference remains intact.
5. Add a dedicated `SIGNAL_CREATE_LIMITER`, separate from `CREATE_LIMITER`.
   Suggested initial anonymous policy: 10 rooms/IP/minute. Treat it as abuse
   dampening, not a precise global quota.
6. Add `PUBLIC_SIGNAL_CREATE` default `false`. Token-authenticated creation can
   be used in controlled environments; set `true` only for the public netsu
   endpoint after rate/observability tests pass.
7. Export `SignalRoom` from `src/index.ts`; keep existing fetch and scheduled
   cleanup behavior unchanged.

**Verify:** existing entry/claim/OpenAPI/health tests and all new route tests
must pass together.

**Commit boundary:** `feat(rendez-key): expose rate-limited signaling routes`

## Task 5: Configure Durable Object migration and generated types

Modify `wrangler.jsonc` with an additive binding and migration:

```jsonc
"durable_objects": {
  "bindings": [
    { "name": "SIGNAL_ROOMS", "class_name": "SignalRoom" }
  ]
},
"migrations": [
  { "tag": "signal-room-v1", "new_sqlite_classes": ["SignalRoom"] }
]
```

Also add `SIGNAL_CREATE_LIMITER` with a unique namespace ID and
`PUBLIC_SIGNAL_CREATE`. Preserve the existing D1 binding, cron, create limiter,
custom domain, compatibility date, and observability settings.

Run:

```bash
bun run --cwd apps/rendez-key types
bun run signal:typecheck
bun run signal:test
bun run signal:deploy:dry
```

Inspect generated type changes and dry-run bindings. The output must list
`SIGNAL_ROOMS` and must not propose a D1 migration.

**Commit boundary:** `build(rendez-key): bind signaling Durable Objects`

## Task 6: Prove local Worker behavior under Wrangler/workerd

1. From the netsu root, start `bun run --cwd apps/rendez-key dev -- --port 8787
--var PUBLIC_SIGNAL_CREATE:true --persist-to <absolute-temp-dir>` in a
   managed child process. `wrangler dev` uses local workerd by default. Wait on
   `/healthz`; never use a fixed sleep.
2. A client-only smoke script creates a room, binds listener/joiner WebSockets,
   sends synthetic offer/answer/candidates, checks exact peer delivery, leaves,
   then verifies reuse fails. The script may run under Node/Bun, but it is only a
   client; the server remains Wrangler/workerd.
3. Repeat ten times and once with the alarm/expiry shortened in a test env.
4. Send SIGTERM and assert Wrangler exits within five seconds with no child
   process or local state leaked into the repo.
5. Scan logs for the known secret/SDP/candidate fixture values; any match fails.

**Verify:**

```bash
bun run --cwd apps/rendez-key dev -- --port 8787 --var PUBLIC_SIGNAL_CREATE:true \
  --persist-to /tmp/netsu-rendez-key-dev-state
# in another shell
node apps/rendez-key/scripts/signal-smoke-test.mjs http://127.0.0.1:8787/v1/signal
```

Automate this flow in tests/CI rather than leaving the manual two-shell form as
the only proof.

**Commit boundary:** `test(rendez-key): smoke signaling under workerd`

## Task 7: Add container and netsu protocol conformance

1. Let netsu's container harness build the in-repository `apps/rendez-key`
   directory and start Wrangler/workerd from the same revision.
2. Publish the signaling golden fixture location/version and make Rust and
   Chromium tests consume or mirror it with a checksum assertion.
3. Run RendezKey Worker tests before netsu transport E2E.
4. Keep signaling fixtures and transport consumers in the same netsu commit. A
   protocol change must update fixtures/version and consumers together.
5. Assert no public egress during E2E and no benchmark payload visible in Worker
   byte counters/logs.

**Commit boundary:** `test(rendez-key): publish signaling conformance fixtures`

## Task 8: Deployment rollout and rollback

1. Deploy first with `PUBLIC_SIGNAL_CREATE=false`. This exercises the DO
   migration without exposing anonymous room creation.
2. Verify existing `/healthz`, D1 create/claim, cron, OpenAPI, and custom domain.
3. Run an authenticated signaling smoke and inspect Worker/DO errors and rate
   metrics. Confirm logs contain no room payload or secret.
4. Enable `PUBLIC_SIGNAL_CREATE=true`; run rate-limit and two-network netsu
   direct smoke tests.
5. Monitor room create/pair/error/expiry counts and DO request duration. Define
   an alert on internal errors and a spend ceiling before announcing the URL.
6. Rollback behavior:
   - disable `PUBLIC_SIGNAL_CREATE` first;
   - clients receive a clear creation-disabled error;
   - existing D1 entry service stays live;
   - do not attempt to delete the DO namespace/migration during incident
     response;
   - revert routes only after active room TTL has elapsed.

Public signaling base after validation (clients derive `wss://` for the room
upgrade):

```text
https://rendez-key.xc.huakun.tech/v1/signal
```

This URL carries signaling only. Successful netsu payload must still select a
direct ICE candidate pair; otherwise netsu aborts.

## Task 9: Documentation and final verification

Update the RendezKey README with an explicit responsibility table:

| Component           | Responsibility                                  | Carries benchmark payload? |
| ------------------- | ----------------------------------------------- | -------------------------- |
| RendezKey Worker/DO | room creation, offer/answer/ICE exchange        | No                         |
| STUN                | public-address discovery/NAT mapping assistance | No                         |
| WebRTC direct pair  | encrypted DataChannel traffic                   | Yes                        |
| TURN                | relay fallback                                  | Unsupported                |

Run:

```bash
cd /Users/hk/Dev/netsu
bun run signal:typecheck
bun run signal:test
bun run signal:deploy:dry
bun run --cwd apps/rendez-key smoke
git diff --check
git status --short
```

Then run the netsu direct/blocked container matrix from its implementation plan.
Do not call the work complete based only on Worker unit tests.

## Acceptance checklist

- [ ] Existing D1 create/claim behavior and tests are unchanged.
- [ ] One code maps to one `SignalRoom` Durable Object.
- [ ] Worker global memory is not authoritative for room/session state.
- [ ] Hibernation handlers and attachments restore socket roles correctly.
- [ ] Alarm expiry and all close/error paths are idempotent.
- [ ] Listener secret is returned once, stored hashed, and never logged/in URL.
- [ ] SDP/candidates are validated/forwarded but never persisted or logged.
- [ ] Per-room and per-IP limits are tested.
- [ ] Local and container servers run under Wrangler/workerd, not Bun APIs.
- [ ] Generated bindings and deploy dry-run include the DO migration.
- [ ] Existing RendezKey tests plus real two-WebSocket tests pass.
- [ ] Public rollout starts disabled and has a clear kill switch.
- [ ] netsu proves direct selected candidates and never sends payload via relay.

## Sources to recheck at execution time

- Durable Object WebSocket server:
  <https://developers.cloudflare.com/durable-objects/examples/websocket-server/>
- WebSocket Hibernation:
  <https://developers.cloudflare.com/durable-objects/best-practices/websockets/>
- Durable Object testing:
  <https://developers.cloudflare.com/durable-objects/examples/testing-with-durable-objects/>
- Wrangler Durable Object bindings/migrations:
  <https://developers.cloudflare.com/durable-objects/get-started/>

Cloudflare runtime APIs can change. Re-run Context7/current official-doc checks
before implementation if the pinned Wrangler version is updated.
