# RendezKey

RendezKey stores a temporary UTF-8 string and returns a short human-safe
code. The string expires automatically and can only be claimed a configured
number of times.

It runs as a Cloudflare Worker (Hono) backed by Cloudflare D1. It is netsu's
public temporary network-test control plane: today it avoids manually copying
long Iroh tickets between test devices and provides the separately versioned
WebRTC signaling surface used to exchange SDP and ICE candidates. The entry API
is not Iroh-specific and can hold any short-lived UTF-8 string.

RendezKey is coordination only. It must never proxy benchmark payload, provide
TURN relay, or become a general-purpose storage service.

| Component                     | Responsibility                                                      | Carries benchmark payload? |
| ----------------------------- | ------------------------------------------------------------------- | -------------------------- |
| Worker + Durable Object       | Create a signaling room and forward offer, answer, and ICE messages | No                         |
| STUN server                   | Tell a peer its public-facing address                               | No                         |
| Direct WebRTC peer connection | Carry the actual netsu benchmark streams                            | Yes                        |
| TURN relay                    | Relay traffic when direct connection fails                          | Unsupported                |

## Security boundary

RendezKey stores plaintext. Do not use it for passwords, private keys,
long-lived credentials, or sensitive production data.

## DDoS and abuse boundary

Cloudflare's network protection absorbs volumetric attacks before the Worker,
but application abuse and cost amplification still require explicit limits:

- `PUBLIC_CREATE=false` is the fail-closed entry-creation switch and production
  default; `PUBLIC_SIGNAL_CREATE=false` independently disables new signaling
  rooms.
- Anonymous mode has lower TTL/read/payload ceilings and a per-IP creation
  limiter. The limiter is local to each Cloudflare location, so it is abuse
  dampening rather than an exact global quota.
- Privileged automation uses `API_TOKEN`; local and PR tests set an arbitrary
  local token, while public smoke workflows read the real token from CI secrets.
- Request-size and parameter checks run before D1 writes; claim predicates
  enforce expiry/read exhaustion even if scheduled cleanup is delayed.
- Cloudflare account budget alerts and an operational procedure to disable
  `PUBLIC_CREATE` are required before enabling a public anonymous deployment.

The anonymous values describe separate limits: TTL is at most one hour, each
code allows at most five claims, and creation is limited to 10 entries per 60
seconds per IP per Cloudflare location. This is not a five-tests-per-hour quota.
Signaling rooms have their own `SIGNAL_CREATE_LIMITER`, also set to 10 room
creates per 60 seconds per IP per Cloudflare location. A local Wrangler test
sets its own token and is not constrained by either anonymous limiter; public
CI should use `API_TOKEN` for the same reason.

## Create tiers (token vs. open mode)

Creating an entry (`POST /v1/entries`) supports two tiers, which coexist:

|                | Trigger                                         | TTL max | reads max | payload max | Rate limit |
| -------------- | ----------------------------------------------- | ------: | --------: | ----------: | ---------- |
| **Privileged** | valid `Authorization: Bearer <API_TOKEN>`       |  7 days |       100 |      64 KiB | none       |
| **Anonymous**  | no `Authorization` header, `PUBLIC_CREATE=true` |  1 hour |         5 |       8 KiB | per-IP     |

Defaults are the same in both tiers (TTL 3600s, 1 read); only the ceilings a
caller may request differ. Claiming is always public and unaffected.

- **`PUBLIC_CREATE`** — the open-mode switch (a plain var, not a secret). Unset
  or any value other than `true`/`1` keeps creation token-only. Set it to
  `true` to also accept unauthenticated (anonymous) creates.
- **`API_TOKEN`** — optional. A purely open deployment can omit it; a
  token-only deployment must set it (otherwise creation is impossible). When
  both `PUBLIC_CREATE=true` and `API_TOKEN` are set, anonymous callers get the
  tight tier while a valid token unlocks the full tier and bypasses the rate
  limit. A **present but invalid** token is rejected with `401` — it is never
  silently downgraded to the anonymous tier.
- **Anonymous rate limit** — anonymous creates are rate-limited per client IP
  (`CF-Connecting-IP`) via Cloudflare's `CREATE_LIMITER` binding
  (`10` requests / `60`s by default; tune in `wrangler.jsonc`). Exceeding it
  returns `429`. The binding is per-datacenter and eventually consistent — it
  dampens abuse, it is not a precise quota.

## Local setup

This package lives in the public netsu Bun workspace. Install once at the netsu
repository root with `bun install`; do not run a second package-local install.

```bash
cd apps/rendez-key
cp .dev.vars.example .dev.vars
bunx wrangler d1 migrations apply rendez-key --local
bun run types
bun run dev
```

From the repository root, the common shortcuts are `bun run signal:dev`,
`bun run signal:test`, `bun run signal:typecheck`, and
`bun run signal:deploy:dry`. Run `bun run signal:test:workerd` for the real
Wrangler/workerd signaling smoke test: it creates and completes ten rooms in
one run, verifies exact offer/answer/ICE forwarding and terminal cleanup, and
scans the server log for leaked SDP, candidates, and listener secrets.
Wrangler/workerd is the server runtime; Bun is the workspace package manager
and script runner.

## Store a string

```bash
CODE=$(curl -fsS -X POST \
  "http://localhost:8787/v1/entries?ttl=3600&reads=1" \
  -H "Authorization: Bearer $RENDEZKEY_TOKEN" \
  -H "Content-Type: text/plain; charset=utf-8" \
  -H "Accept: text/plain" \
  --data-binary "$IROH_TICKET")
```

`$RENDEZKEY_TOKEN` must match the `API_TOKEN` secret configured for the
Worker. On a deployment with `PUBLIC_CREATE=true` you can omit the
`Authorization` header entirely to create under the anonymous tier (tighter
caps, per-IP rate limited — see [Create tiers](#create-tiers-token-vs-open-mode)).
The `Accept: text/plain` header returns the code as a bare string; omit it (or
send `Accept: application/json`) to get a JSON body instead — see below.

## Claim a string

```bash
curl -fsS -X POST \
  "http://localhost:8787/v1/entries/$CODE/claim"
```

Claiming does not require an API token — the short code itself is a
temporary bearer capability. The response body is the original string.

## Deploy

```bash
bunx wrangler d1 create rendez-key
bunx wrangler d1 migrations apply rendez-key --remote
bunx wrangler secret put API_TOKEN   # optional in open mode (see below)
bun run types
bun run test
bun run deploy:dry
bun run deploy
```

The checked-in `wrangler.jsonc` retains netsu's current public custom domain and
D1 database ID so the existing deployment can be managed from its new source
owner. These identifiers are not credentials. Forks must create their own D1
database, replace the ID/domain, and set secrets through Wrangler before deploy.

To deploy a **token-less (open) instance**, set `PUBLIC_CREATE` to `"true"` in
[`wrangler.jsonc`](./wrangler.jsonc)'s `vars` (default is `"false"`) and deploy.
The `CREATE_LIMITER` rate-limit binding is already declared there; adjust its
`simple.limit` / `period` to taste. `API_TOKEN` is optional for an open
instance — omit `wrangler secret put API_TOKEN` to run purely anonymous, or set
it to additionally offer the privileged tier.

`PUBLIC_SIGNAL_CREATE` is a separate switch for unauthenticated signaling-room
creation. Its `SIGNAL_CREATE_LIMITER` binding can be tuned independently from
the entry limiter. Keep both public switches false when only token-authenticated
automation should create resources.

## Post-deploy smoke test

After a deploy, verify both live surfaces end-to-end: store/claim an entry, then
complete the signaling handshake and confirm a terminal room cannot be reused.

```bash
BASE_URL="https://<your-worker>.workers.dev" \
RENDEZKEY_TOKEN="<matches the API_TOKEN secret>" \
bun run smoke
```

## API reference

### `GET /healthz`

No authentication. Returns `200 OK` with `{ "status": "ok", "service": "rendezkey" }`.
Does not touch D1.

### `POST /v1/entries`

Creates an entry. Requires `Authorization: Bearer <API_TOKEN>` (privileged
tier), or — when the deployment sets `PUBLIC_CREATE=true` — no auth at all
(anonymous tier). See [Create tiers](#create-tiers-token-vs-open-mode) for the
per-tier ceilings and rate limiting.

Query parameters (Max shown is the privileged ceiling; the anonymous tier caps
`ttl` at 3600 and `reads` at 5):

| Parameter | Meaning                   | Default | Min |             Max |
| --------- | ------------------------- | ------: | --: | --------------: |
| `ttl`     | Time to live, in seconds  |    3600 |  60 | 604800 (7 days) |
| `reads`   | Maximum successful claims |       1 |   1 |             100 |

Request headers:

- `Content-Type: text/plain; charset=utf-8` (required — any other
  content type is rejected).
- `Accept: text/plain` (optional — selects the plain-text response
  variant below; otherwise a JSON body is returned).

Body: the raw UTF-8 string to store, between 1 and 65536 bytes.

JSON response (default, `Accept` absent or not `text/plain`):

```http
201 Created
Content-Type: application/json
```

```json
{
  "code": "7K3M-Q9TX",
  "expires_at": "2026-07-20T16:00:00.000Z",
  "max_reads": 1
}
```

Plain-text response (`Accept: text/plain`):

```http
201 Created
Content-Type: text/plain; charset=utf-8
X-RendezKey-Expires-At: 2026-07-20T16:00:00.000Z
X-RendezKey-Max-Reads: 1
```

```text
7K3M-Q9TX
```

### `POST /v1/entries/:code/claim`

Claims (and atomically decrements the remaining-reads counter for) an
entry. No authentication required — the short code is the capability.
Empty request body.

Success:

```http
200 OK
Content-Type: text/plain; charset=utf-8
Cache-Control: no-store
X-RendezKey-Remaining-Reads: 0
X-RendezKey-Expires-At: 2026-07-20T16:00:00.000Z
```

The response body is the exact string that was uploaded.

An entry that is missing, malformed, expired, or already exhausted all
return the same `404` shape, to avoid leaking which of those states
applies:

```http
404 Not Found
Content-Type: application/problem+json
```

```json
{
  "type": "https://rendezkey.dev/problems/entry_not_available",
  "title": "Entry not available",
  "status": 404,
  "code": "entry_not_available"
}
```

## API docs (OpenAPI + Scalar)

- `GET /openapi.json` — generated OpenAPI 3.1 document (via
  [`hono-openapi`](https://github.com/rhinobase/hono-openapi)).
- `GET /docs` — interactive [Scalar](https://scalar.com) API reference UI,
  reading the spec above. Open `http://localhost:8787/docs` locally, or
  `https://<your-worker>.workers.dev/docs` once deployed, to browse and
  try requests from the browser.

## hono/client RPC

All routes are chained on a single `Hono` instance and the composed type
is exported as `AppType`, so a TypeScript caller gets a fully-typed
[`hono/client`](https://hono.dev/docs/guides/rpc) without duplicating the
API contract by hand:

```ts
import { hc } from "hono/client";
import type { AppType } from "rendez-key"; // or "../src/client" in-repo

const client = hc<AppType>("https://<your-worker>.workers.dev");

const created = await client["v1"]["entries"].$post(
  { query: { reads: "3" } },
  {
    init: { body: "my-ticket", headers: { Authorization: `Bearer ${token}` } },
  },
);

const claimed = await client["v1"]["entries"][":code"]["claim"].$post({
  param: { code: "7K3M-Q9TX" },
});
```

The create endpoint negotiates JSON vs. plain-text response bodies at the
same status code via the `Accept` header — Hono's RPC types can't express
that discrimination, so `.json()` on that one call needs an explicit type
assertion (see `test/rpc-client.test.ts` for a worked example). Every
other route's response is fully typed end-to-end.

Response bodies and path params are typed; **query params are not** —
`ttl`/`reads` are validated manually in `src/domain/limits.ts` rather
than through a `validator()` middleware (to avoid touching that
already-reviewed logic), so the RPC input type for their query object is
an unconstrained `{}` and `client["v1"]["entries"].$post({ query: {...} })`
will accept any keys without a compile error. Values are still validated
at runtime exactly as documented above.

### Publishing just the type

This package ships as a Cloudflare Worker, not an npm library — but other
projects can still get `AppType` for `hono/client` without pulling in the
server implementation. `bun run build:types` (also runs automatically via
`prepublishOnly`) emits declaration-only output (`.d.ts` files, no
implementation, no `.js`) to `dist/`, and `package.json`'s `files`/`types`/
`exports` fields restrict what `npm publish` would ship to that `dist/`
directory — `src/` itself is never published. `hono` is a
`peerDependency` (the only runtime package a consumer actually needs to
use `hc<AppType>()`); everything else (`drizzle-orm`, `hono-openapi`,
`zod`, `@scalar/hono-api-reference`, …) is a `devDependency`, since it's
only needed to build and run the Worker, not to consume the published
type. Note this package is currently `"private": true` — flip that (and
bump the version) before actually running `npm publish`.

Because Hono's route types are structurally composed, the shipped `dist/`
mirrors the module structure (`app.d.ts`, `repositories/entries.d.ts`,
`db/schema.d.ts`, `domain/limits.d.ts`, …) rather than a single rolled-up
file — that's more of the internal shape than just "`typeof app`," though
every file is declarations only (no implementation, no function bodies).
A single-file bundle was deliberately not attempted: dts-bundling tools
are known to be fragile against Hono's complex generic types, and getting
that wrong would be worse than shipping a slightly wider (but still
implementation-free) surface.

`dist/worker-configuration.d.ts` (the wrangler-generated ambient types
`AppType` depends on for `D1Database`/`CloudflareBindings`/etc.) is
copied alongside and referenced automatically. Consumers need a
`tsconfig` **without `"dom"` in `lib`** (workerd's runtime types
redeclare `Response`/`Request`/`EventTarget`/etc. and collide with the
browser DOM lib — the same constraint that applies to Cloudflare's own
`@cloudflare/workers-types`, not something specific to this package) and
`"skipLibCheck": true` (standard practice for any project consuming
third-party ambient types; without it, one stray internal reference in
the generated file — `Cloudflare.GlobalProps.mainModule`, which no RPC
consumer touches — fails to resolve).

## WebRTC signaling API

### `POST /v1/signal/rooms`

Creates a single-use signaling room and returns `201 Created` with its short
code, expiry, and a listener secret. Creation requires a valid
`Authorization: Bearer <API_TOKEN>`, unless `PUBLIC_SIGNAL_CREATE=true` enables
the separately rate-limited anonymous tier. Signaling rooms expire after five
minutes; entry TTL and claim-count limits do not apply to them.

The listener secret is returned once and only its SHA-256 digest is stored. Do
not put the secret in URLs or logs. The create response is intentionally marked
`Cache-Control: no-store`.

### `GET /v1/signal/rooms/:code/ws`

Upgrades to a WebSocket. The first client message must bind the socket as
either the room's `listener` (with the listener secret) or its one `joiner`.
After both peers bind, the room forwards only validated protocol-v1 control
messages in this order:

1. joiner sends one SDP offer;
2. listener sends one SDP answer;
3. either peer may send bounded ICE candidates and one end-of-candidates marker.

The Durable Object uses WebSocket hibernation, persists only minimal room
metadata, caps each message at 64 KiB and the room transcript at 1 MiB, and
allows at most 16 ICE candidates per peer. Disconnect, protocol error, expiry,
or successful terminal use closes the room permanently; a new benchmark must
create a new room.

This service never receives WebRTC data-channel payload. No TURN URLs are
accepted or returned: if STUN-assisted direct connectivity fails, netsu stops
the WebRTC run with a bounded warning instead of falling back to relay traffic.

## Limits

Maximums are per-tier — the privileged (token) ceilings are shown here; the
anonymous (open-mode) tier caps TTL at 3600s, reads at 5, and payload at 8 KiB.

| Limit               |      Default |        Min |        Max (privileged) |
| ------------------- | -----------: | ---------: | ----------------------: |
| TTL (`ttl`)         | 3600 seconds | 60 seconds | 604800 seconds (7 days) |
| Max reads (`reads`) |            1 |          1 |                     100 |
| Payload size        |            — |     1 byte |    65536 bytes (64 KiB) |

## Response / error codes

Error responses use `application/problem+json` and never include stack
traces, D1 SQL, or secrets.

| HTTP | `code`                   | Scenario                                                                |
| ---: | ------------------------ | ----------------------------------------------------------------------- |
|  200 | —                        | Entry claimed successfully                                              |
|  201 | —                        | Entry created successfully                                              |
|  400 | `invalid_request`        | Invalid `ttl`, `reads`, content type, or empty body                     |
|  401 | `unauthorized`           | Missing/incorrect token (closed mode), or an invalid token in open mode |
|  404 | `entry_not_available`    | Code invalid, unknown, expired, or exhausted                            |
|  413 | `payload_too_large`      | UTF-8 body exceeds the tier's payload ceiling                           |
|  429 | `rate_limited`           | Anonymous per-IP create rate limit exceeded (open mode)                 |
|  500 | `internal_error`         | Unclassified server error                                               |
|  503 | `code_generation_failed` | 5 consecutive code collisions                                           |

## Cleanup

A scheduled (cron) handler runs hourly and deletes all expired or exhausted
entries in a single statement. Claim checks are always re-validated against
`expires_at` and `remaining_reads` at request time, so a missed or
delayed cleanup run cannot make expired/exhausted data reappear as
claimable — cleanup only reclaims storage.
