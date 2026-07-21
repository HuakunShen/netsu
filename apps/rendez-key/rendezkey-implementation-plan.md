# RendezKey Implementation Plan

> **Historical v1 record:** This plan describes the already-implemented D1
> entry service in its original workspace. For current Bun commands, public
> repository ownership, abuse controls, and future WebRTC signaling, use
> `README.md` and the active plans under the netsu repository `docs/plans/`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and deploy a minimal Cloudflare Worker that stores a temporary UTF-8 string in D1, returns an 8-character human-safe code, and atomically allows the string to be claimed a configurable number of times before expiry.

**Architecture:** A Hono-based Cloudflare Worker exposes create, claim, and health endpoints. D1 stores plaintext entries and performs transactional read-count decrements. A Cron Trigger removes expired and exhausted entries; request-time predicates enforce expiry even when cleanup is delayed.

**Tech Stack:** TypeScript, Hono, Cloudflare Workers, Cloudflare D1, Wrangler v4+, Vitest, `@cloudflare/vitest-pool-workers`.

## Global Constraints

- Working name: `RendezKey`.
- Store UTF-8 text only.
- Payload size: 1–65,536 UTF-8 bytes.
- Code alphabet: `23456789ABCDEFGHJKLMNPQRSTUVWXYZ`.
- Code length: exactly 8 normalized characters; display as `XXXX-XXXX`.
- Code generation: `crypto.getRandomValues()` only.
- Default TTL: 3,600 seconds.
- TTL range: 60–604,800 seconds.
- Default maximum reads: 1.
- Reads range: 1–100.
- Create endpoint requires `Authorization: Bearer <API_TOKEN>`.
- Claim endpoint requires only the code.
- D1 is the only persistence layer.
- No KV, Durable Objects, Cache API, WebSocket, E2EE, account system, or web UI.
- Every response includes `Cache-Control: no-store`.
- Logs must never contain payload, authorization header, API token, or full code.
- Use `wrangler.jsonc`, not `wrangler.toml`.
- Use the D1 binding, never the Cloudflare REST API from inside the Worker.
- Generate Worker binding types with `wrangler types`; do not hand-write the `Env` interface.
- Set `compatibility_date` to `2026-07-20`.
- Enable Workers observability with structured JSON logging.
- Every Promise must be awaited, returned, explicitly voided, or passed to `ctx.waitUntil()`.
- Follow TDD: failing test, minimal implementation, passing test, commit.

---

## Planned File Structure

```text
rendezkey/
├── migrations/
│   └── 0001_create_entries.sql
├── src/
│   ├── app.ts
│   ├── index.ts
│   ├── scheduled.ts
│   ├── domain/
│   │   ├── code.ts
│   │   └── limits.ts
│   ├── http/
│   │   ├── auth.ts
│   │   ├── errors.ts
│   │   └── headers.ts
│   ├── repositories/
│   │   └── entries.ts
│   └── routes/
│       ├── claim-entry.ts
│       ├── create-entry.ts
│       └── health.ts
├── test/
│   ├── apply-migrations.ts
│   ├── env.d.ts
│   ├── health.test.ts
│   ├── code.test.ts
│   ├── create-entry.test.ts
│   ├── claim-entry.test.ts
│   ├── concurrent-claim.test.ts
│   └── scheduled.test.ts
├── .dev.vars.example
├── .gitignore
├── package.json
├── README.md
├── tsconfig.json
├── vitest.config.ts
├── worker-configuration.d.ts
└── wrangler.jsonc
```

### Responsibility Map

| File | Responsibility |
|---|---|
| `src/index.ts` | Export Worker `fetch` and `scheduled` handlers |
| `src/app.ts` | Construct Hono app and register middleware/routes |
| `src/domain/code.ts` | Generate, normalize, validate, and format codes |
| `src/domain/limits.ts` | Parse and validate TTL, reads, and payload byte size |
| `src/http/auth.ts` | Bearer token extraction and timing-safe comparison |
| `src/http/errors.ts` | RFC 9457-style problem responses |
| `src/http/headers.ts` | Shared no-store and response header helpers |
| `src/repositories/entries.ts` | All D1 SQL; create, claim, and cleanup |
| `src/routes/create-entry.ts` | Create endpoint HTTP concerns |
| `src/routes/claim-entry.ts` | Claim endpoint HTTP concerns |
| `src/routes/health.ts` | Health endpoint |
| `src/scheduled.ts` | Cron cleanup orchestration |
| `migrations/0001_create_entries.sql` | D1 schema |
| `test/*` | Unit and Worker integration tests |

---

### Task 1: Scaffold the Cloudflare Worker, D1 Schema, and Current Test Runtime

**Files:**
- Create: `package.json`
- Create: `tsconfig.json`
- Create: `wrangler.jsonc`
- Create: `vitest.config.ts`
- Create: `.gitignore`
- Create: `.dev.vars.example`
- Create: `migrations/0001_create_entries.sql`
- Create: `test/apply-migrations.ts`
- Create: `test/env.d.ts`
- Create: `src/index.ts`
- Create: `src/app.ts`
- Create: `src/routes/health.ts`
- Create: `src/scheduled.ts`
- Generate: `worker-configuration.d.ts`
- Test: `test/health.test.ts`

**Interfaces:**
- Produces: a D1 binding named `DB`.
- Produces: a required Worker secret binding named `API_TOKEN`.
- Produces: `createApp(): Hono<{ Bindings: CloudflareBindings }>`
- Produces: Worker default export with `fetch` and `scheduled` handlers.
- Produces: `GET /healthz`.
- Produces: a test database whose migrations are applied automatically.

- [ ] **Step 1: Initialize the project and install current dependencies**

Run:

```bash
mkdir rendezkey
cd rendezkey
npm init -y
npm install hono
npm install -D wrangler@latest typescript \
  vitest@^4.1.0 @cloudflare/vitest-pool-workers
npm pkg set type=module
npm pkg set private=true --json
npm pkg set scripts.dev="wrangler dev"
npm pkg set scripts.types="wrangler types"
npm pkg set scripts.typecheck="tsc --noEmit"
npm pkg set scripts.test="vitest run"
npm pkg set scripts.test:watch="vitest"
npm pkg set scripts.deploy:dry="wrangler deploy --dry-run"
npm pkg set scripts.deploy="wrangler deploy"
npm pkg set scripts.tail="wrangler tail"
```

Expected: `package.json` and lockfile exist; Vitest is version 4.1 or later; all commands exit with code 0.

- [ ] **Step 2: Create the base `wrangler.jsonc`**

```jsonc
{
  "$schema": "./node_modules/wrangler/config-schema.json",
  "name": "rendezkey",
  "main": "src/index.ts",
  "compatibility_date": "2026-07-20",
  "compatibility_flags": ["nodejs_compat"],
  "secrets": {
    "required": ["API_TOKEN"]
  },
  "triggers": {
    "crons": ["0 * * * *"]
  },
  "observability": {
    "enabled": true,
    "head_sampling_rate": 1
  }
}
```

- [ ] **Step 3: Create D1 and let Wrangler write the concrete binding**

Authenticate if required:

```bash
npx wrangler whoami
```

Create the database and update the config automatically:

```bash
npx wrangler d1 create rendezkey \
  --binding DB \
  --update-config
```

Expected: Wrangler writes a `d1_databases` entry containing the real `database_name` and `database_id` into `wrangler.jsonc`. Add `"migrations_dir": "./migrations"` to that generated object without changing the generated ID.

- [ ] **Step 4: Create the initial D1 migration**

Create `migrations/0001_create_entries.sql`:

```sql
CREATE TABLE entries (
  code TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  max_reads INTEGER NOT NULL CHECK (max_reads BETWEEN 1 AND 100),
  remaining_reads INTEGER NOT NULL CHECK (remaining_reads >= 0),
  last_claim_id TEXT
);

CREATE INDEX idx_entries_cleanup
ON entries (expires_at, remaining_reads);
```

- [ ] **Step 5: Create `tsconfig.json`**

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "lib": ["ES2022", "WebWorker"],
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "noImplicitOverride": true,
    "noFallthroughCasesInSwitch": true,
    "skipLibCheck": true,
    "types": ["@cloudflare/vitest-pool-workers/types"]
  },
  "include": [
    "src/**/*.ts",
    "test/**/*.ts",
    "vitest.config.ts",
    "worker-configuration.d.ts"
  ]
}
```

- [ ] **Step 6: Configure the current Cloudflare Vitest plugin**

Create `vitest.config.ts`:

```ts
import { fileURLToPath } from "node:url";
import {
  cloudflareTest,
  readD1Migrations,
} from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig(async () => {
  const migrationsPath = fileURLToPath(
    new URL("./migrations", import.meta.url),
  );
  const migrations = await readD1Migrations(migrationsPath);

  return {
    plugins: [
      cloudflareTest({
        wrangler: {
          configPath: "./wrangler.jsonc",
        },
        miniflare: {
          bindings: {
            API_TOKEN: "test-token",
            TEST_MIGRATIONS: migrations,
          },
        },
      }),
    ],
    test: {
      setupFiles: ["./test/apply-migrations.ts"],
    },
  };
});
```

This uses the current `cloudflareTest()` plugin. Do not use the deprecated Vitest 3 `cloudflareTest()` API.

- [ ] **Step 7: Apply D1 migrations automatically in tests**

Create `test/apply-migrations.ts`:

```ts
import { applyD1Migrations } from "cloudflare:test";
import { env } from "cloudflare:workers";

await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
```

Create `test/env.d.ts`:

```ts
declare module "cloudflare:workers" {
  interface ProvidedEnv extends CloudflareBindings {
    TEST_MIGRATIONS: D1Migration[];
  }
}

export {};
```

- [ ] **Step 8: Create `.gitignore` and `.dev.vars.example`**

`.gitignore`:

```gitignore
node_modules/
.wrangler/
.dev.vars
.env
coverage/
dist/
*.log
```

`.dev.vars.example`:

```dotenv
API_TOKEN=replace-with-a-long-random-development-token
```

- [ ] **Step 9: Write the failing health test**

Create `test/health.test.ts`:

```ts
import { exports } from "cloudflare:workers";
import { describe, expect, it } from "vitest";

describe("GET /healthz", () => {
  it("returns service health without touching D1", async () => {
    const response = await exports.default.fetch(
      "https://example.test/healthz",
    );

    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    await expect(response.json()).resolves.toEqual({
      status: "ok",
      service: "rendezkey",
    });
  });
});
```

- [ ] **Step 10: Run the test and verify failure**

Run:

```bash
npm run types
npm test -- test/health.test.ts
```

Expected: FAIL because the Worker app and route do not exist.

- [ ] **Step 11: Implement the health route and app**

Create `src/routes/health.ts`:

```ts
import type { Context } from "hono";

export function health(c: Context): Response {
  return c.json(
    {
      status: "ok",
      service: "rendezkey",
    },
    200,
  );
}
```

Create `src/app.ts`:

```ts
import { Hono } from "hono";
import { health } from "./routes/health";

export function createApp() {
  const app = new Hono<{ Bindings: CloudflareBindings }>();

  app.use("*", async (c, next) => {
    await next();
    c.res.headers.set("Cache-Control", "no-store");
  });

  app.get("/healthz", health);

  return app;
}
```

Create `src/scheduled.ts`:

```ts
export async function runCleanup(
  _env: CloudflareBindings,
  _scheduledTimeMs: number,
): Promise<void> {}
```

Create `src/index.ts`:

```ts
import { createApp } from "./app";
import { runCleanup } from "./scheduled";

const app = createApp();

export default {
  fetch: app.fetch,
  scheduled(
    controller: ScheduledController,
    env: CloudflareBindings,
    ctx: ExecutionContext,
  ): void {
    ctx.waitUntil(runCleanup(env, controller.scheduledTime));
  },
} satisfies ExportedHandler<CloudflareBindings>;
```

- [ ] **Step 12: Run migrations locally, regenerate types, and verify**

```bash
cp .dev.vars.example .dev.vars
npx wrangler d1 migrations apply rendezkey --local
npm run types
npm test -- test/health.test.ts
npm run typecheck
```

Expected: migration succeeds, health test PASS, and TypeScript exits with code 0.

- [ ] **Step 13: Commit Task 1**

```bash
git add package.json package-lock.json tsconfig.json wrangler.jsonc \
  vitest.config.ts .gitignore .dev.vars.example migrations src test \
  worker-configuration.d.ts
git commit -m "chore: scaffold rendezkey worker"
```

---

### Task 2: Implement Human-Safe Code Generation and Validation

**Files:**
- Create: `src/domain/code.ts`
- Test: `test/code.test.ts`

**Interfaces:**
- Produces: `generateCode(): string` returning 8 normalized characters.
- Produces: `formatCode(code: string): string`.
- Produces: `normalizeCode(input: string): string | null`.
- Produces: `CODE_ALPHABET` and `CODE_LENGTH`.

- [ ] **Step 1: Write the failing code-domain tests**

Create `test/code.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import {
  CODE_ALPHABET,
  formatCode,
  generateCode,
  normalizeCode,
} from "../src/domain/code";

describe("code domain", () => {
  it("generates eight characters from the human-safe alphabet", () => {
    for (let index = 0; index < 100; index += 1) {
      const code = generateCode();
      expect(code).toHaveLength(8);
      for (const character of code) {
        expect(CODE_ALPHABET).toContain(character);
      }
    }
  });

  it("formats normalized code as four-four", () => {
    expect(formatCode("7K3MQ9TX")).toBe("7K3M-Q9TX");
  });

  it("normalizes case, spaces, and hyphens", () => {
    expect(normalizeCode(" 7k3m-q9tx ")).toBe("7K3MQ9TX");
    expect(normalizeCode("7K3M Q9TX")).toBe("7K3MQ9TX");
  });

  it("rejects invalid length and alphabet characters", () => {
    expect(normalizeCode("ABC")).toBeNull();
    expect(normalizeCode("0000-0000")).toBeNull();
    expect(normalizeCode("IIII-IIII")).toBeNull();
  });
});
```

- [ ] **Step 2: Run tests and verify failure**

```bash
npm test -- test/code.test.ts
```

Expected: FAIL because `src/domain/code.ts` does not exist.

- [ ] **Step 3: Implement `src/domain/code.ts`**

```ts
export const CODE_ALPHABET = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
export const CODE_LENGTH = 8;

export function generateCode(): string {
  const randomBytes = crypto.getRandomValues(new Uint8Array(5));
  let randomValue = 0n;

  for (const byte of randomBytes) {
    randomValue = (randomValue << 8n) | BigInt(byte);
  }

  let output = "";

  for (let shift = 35n; shift >= 0n; shift -= 5n) {
    const alphabetIndex = Number((randomValue >> shift) & 31n);
    output += CODE_ALPHABET[alphabetIndex];
  }

  if (output.length !== CODE_LENGTH) {
    throw new Error("invalid_code_generation_state");
  }

  return output;
}

export function formatCode(code: string): string {
  return `${code.slice(0, 4)}-${code.slice(4)}`;
}

export function normalizeCode(input: string): string | null {
  const normalized = input.toUpperCase().replace(/[\s-]+/g, "");

  if (normalized.length !== CODE_LENGTH) {
    return null;
  }

  for (const character of normalized) {
    if (!CODE_ALPHABET.includes(character)) {
      return null;
    }
  }

  return normalized;
}
```

- [ ] **Step 4: Run tests and typecheck**

```bash
npm test -- test/code.test.ts
npm run typecheck
```

Expected: all code-domain tests PASS and typecheck exits with code 0.

- [ ] **Step 5: Commit Task 2**

```bash
git add src/domain/code.ts test/code.test.ts
git commit -m "feat: add human-safe short codes"
```

---

### Task 3: Add Request Limits, Problem Responses, and Authentication

**Files:**
- Create: `src/domain/limits.ts`
- Create: `src/http/auth.ts`
- Create: `src/http/errors.ts`
- Create: `src/http/headers.ts`
- Test: `test/http-helpers.test.ts`

**Interfaces:**
- Produces: `parseTtl(raw: string | undefined): number`.
- Produces: `parseMaxReads(raw: string | undefined): number`.
- Produces: `readUtf8Body(request: Request): Promise<string>`.
- Produces: `requireCreateAuth(c, next): Promise<Response | void>`.
- Produces: `problem(c, status, code, title, detail?)`.

- [ ] **Step 1: Write failing helper tests**

Create `test/http-helpers.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import {
  MAX_PAYLOAD_BYTES,
  parseMaxReads,
  parseTtl,
  utf8ByteLength,
} from "../src/domain/limits";

describe("request limits", () => {
  it("uses defaults", () => {
    expect(parseTtl(undefined)).toBe(3600);
    expect(parseMaxReads(undefined)).toBe(1);
  });

  it("accepts inclusive boundaries", () => {
    expect(parseTtl("60")).toBe(60);
    expect(parseTtl("604800")).toBe(604800);
    expect(parseMaxReads("1")).toBe(1);
    expect(parseMaxReads("100")).toBe(100);
  });

  it("rejects invalid values", () => {
    expect(() => parseTtl("59")).toThrow("invalid_ttl");
    expect(() => parseTtl("1.5")).toThrow("invalid_ttl");
    expect(() => parseMaxReads("0")).toThrow("invalid_reads");
    expect(() => parseMaxReads("101")).toThrow("invalid_reads");
  });

  it("counts UTF-8 bytes instead of JavaScript characters", () => {
    expect(utf8ByteLength("a")).toBe(1);
    expect(utf8ByteLength("中")).toBe(3);
    expect(MAX_PAYLOAD_BYTES).toBe(65_536);
  });
});
```

- [ ] **Step 2: Run tests and verify failure**

```bash
npm test -- test/http-helpers.test.ts
```

Expected: FAIL because helper modules do not exist.

- [ ] **Step 3: Implement `src/domain/limits.ts`**

```ts
export const DEFAULT_TTL_SECONDS = 3_600;
export const MIN_TTL_SECONDS = 60;
export const MAX_TTL_SECONDS = 604_800;

export const DEFAULT_MAX_READS = 1;
export const MIN_MAX_READS = 1;
export const MAX_MAX_READS = 100;

export const MAX_PAYLOAD_BYTES = 65_536;

function parseBoundedInteger(
  raw: string | undefined,
  defaultValue: number,
  minimum: number,
  maximum: number,
  errorCode: string,
): number {
  if (raw === undefined) {
    return defaultValue;
  }

  if (!/^\d+$/.test(raw)) {
    throw new Error(errorCode);
  }

  const value = Number(raw);

  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(errorCode);
  }

  return value;
}

export function parseTtl(raw: string | undefined): number {
  return parseBoundedInteger(
    raw,
    DEFAULT_TTL_SECONDS,
    MIN_TTL_SECONDS,
    MAX_TTL_SECONDS,
    "invalid_ttl",
  );
}

export function parseMaxReads(raw: string | undefined): number {
  return parseBoundedInteger(
    raw,
    DEFAULT_MAX_READS,
    MIN_MAX_READS,
    MAX_MAX_READS,
    "invalid_reads",
  );
}

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

export async function readUtf8Body(request: Request): Promise<string> {
  const declaredLength = request.headers.get("content-length");

  if (
    declaredLength !== null &&
    Number.isFinite(Number(declaredLength)) &&
    Number(declaredLength) > MAX_PAYLOAD_BYTES
  ) {
    throw new Error("payload_too_large");
  }

  const value = await request.text();
  const byteLength = utf8ByteLength(value);

  if (byteLength < 1) {
    throw new Error("empty_payload");
  }

  if (byteLength > MAX_PAYLOAD_BYTES) {
    throw new Error("payload_too_large");
  }

  return value;
}
```

- [ ] **Step 4: Implement problem responses**

Create `src/http/errors.ts`:

```ts
import type { Context } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";

export function problem(
  c: Context,
  status: ContentfulStatusCode,
  code: string,
  title: string,
  detail?: string,
): Response {
  c.header("Content-Type", "application/problem+json");
  c.header("Cache-Control", "no-store");

  return c.json(
    {
      type: `https://rendezkey.dev/problems/${code}`,
      title,
      status,
      code,
      ...(detail === undefined ? {} : { detail }),
    },
    status,
  );
}
```

Create `src/http/headers.ts`:

```ts
export function applyNoStore(headers: Headers): void {
  headers.set("Cache-Control", "no-store");
}
```

- [ ] **Step 5: Implement timing-safe create authentication**

Create `src/http/auth.ts`:

```ts
import type { MiddlewareHandler } from "hono";
import { problem } from "./errors";

const encoder = new TextEncoder();

function readBearerToken(headerValue: string | undefined): string | null {
  if (headerValue === undefined) {
    return null;
  }

  const match = /^Bearer ([^\s]+)$/.exec(headerValue);
  return match?.[1] ?? null;
}

function timingSafeStringEqual(left: string, right: string): boolean {
  const leftBytes = encoder.encode(left);
  const rightBytes = encoder.encode(right);

  if (leftBytes.byteLength !== rightBytes.byteLength) {
    return false;
  }

  return crypto.subtle.timingSafeEqual(leftBytes, rightBytes);
}

export const requireCreateAuth: MiddlewareHandler<{
  Bindings: CloudflareBindings;
}> = async (c, next) => {
  const supplied = readBearerToken(c.req.header("Authorization"));

  if (
    supplied === null ||
    !timingSafeStringEqual(supplied, c.env.API_TOKEN)
  ) {
    return problem(c, 401, "unauthorized", "Unauthorized");
  }

  await next();
};
```

- [ ] **Step 6: Run helper tests and typecheck**

```bash
npm test -- test/http-helpers.test.ts
npm run typecheck
```

Expected: tests PASS and typecheck exits with code 0.

- [ ] **Step 7: Commit Task 3**

```bash
git add src/domain/limits.ts src/http test/http-helpers.test.ts
git commit -m "feat: add request validation and auth"
```

---

### Task 4: Implement the D1 Entry Repository

**Files:**
- Create: `src/repositories/entries.ts`
- Test: `test/repository.test.ts`

**Interfaces:**
- Produces: `createEntry(db, input): Promise<StoredEntry>`.
- Produces: `claimEntry(db, input): Promise<ClaimedEntry | null>`.
- Produces: `cleanupEntries(db, nowSeconds, limit): Promise<number>`.

- [ ] **Step 1: Write failing repository tests**

Create `test/repository.test.ts`:

```ts
import { env } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";
import {
  claimEntry,
  cleanupEntries,
  createEntry,
} from "../src/repositories/entries";

describe("entries repository", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("creates and claims an entry once", async () => {
    await createEntry(env.DB, {
      code: "7K3MQ9TX",
      value: "ticket",
      nowSeconds: 1000,
      expiresAtSeconds: 4600,
      maxReads: 1,
    });

    const first = await claimEntry(env.DB, {
      code: "7K3MQ9TX",
      nowSeconds: 1001,
      claimId: "claim-a",
    });

    const second = await claimEntry(env.DB, {
      code: "7K3MQ9TX",
      nowSeconds: 1002,
      claimId: "claim-b",
    });

    expect(first).toEqual({
      value: "ticket",
      remainingReads: 0,
      expiresAtSeconds: 4600,
    });
    expect(second).toBeNull();
  });

  it("rejects expired entries", async () => {
    await createEntry(env.DB, {
      code: "7K3MQ9TX",
      value: "ticket",
      nowSeconds: 1000,
      expiresAtSeconds: 1060,
      maxReads: 1,
    });

    await expect(
      claimEntry(env.DB, {
        code: "7K3MQ9TX",
        nowSeconds: 1060,
        claimId: "claim-a",
      }),
    ).resolves.toBeNull();
  });

  it("cleans expired and exhausted rows", async () => {
    await createEntry(env.DB, {
      code: "7K3MQ9TX",
      value: "expired",
      nowSeconds: 1000,
      expiresAtSeconds: 1010,
      maxReads: 1,
    });

    await createEntry(env.DB, {
      code: "ABCDEFGH",
      value: "active",
      nowSeconds: 1000,
      expiresAtSeconds: 5000,
      maxReads: 1,
    });

    const deleted = await cleanupEntries(env.DB, 2000, 1000);
    expect(deleted).toBe(1);
  });
});
```

- [ ] **Step 2: Run repository tests and verify failure**

```bash
npm test -- test/repository.test.ts
```

Expected: FAIL because repository functions do not exist.

- [ ] **Step 3: Implement `src/repositories/entries.ts`**

```ts
export interface CreateEntryInput {
  code: string;
  value: string;
  nowSeconds: number;
  expiresAtSeconds: number;
  maxReads: number;
}

export interface StoredEntry {
  code: string;
  expiresAtSeconds: number;
  maxReads: number;
}

export interface ClaimEntryInput {
  code: string;
  nowSeconds: number;
  claimId: string;
}

export interface ClaimedEntry {
  value: string;
  remainingReads: number;
  expiresAtSeconds: number;
}

interface ClaimRow {
  value: string;
  remaining_reads: number;
  expires_at: number;
}

export async function createEntry(
  db: D1Database,
  input: CreateEntryInput,
): Promise<StoredEntry> {
  await db
    .prepare(
      `INSERT INTO entries (
        code,
        value,
        created_at,
        expires_at,
        max_reads,
        remaining_reads,
        last_claim_id
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, NULL)`,
    )
    .bind(
      input.code,
      input.value,
      input.nowSeconds,
      input.expiresAtSeconds,
      input.maxReads,
    )
    .run();

  return {
    code: input.code,
    expiresAtSeconds: input.expiresAtSeconds,
    maxReads: input.maxReads,
  };
}

export async function claimEntry(
  db: D1Database,
  input: ClaimEntryInput,
): Promise<ClaimedEntry | null> {
  const [updateResult, selectResult] = await db.batch<ClaimRow>([
    db
      .prepare(
        `UPDATE entries
         SET
           remaining_reads = remaining_reads - 1,
           last_claim_id = ?1
         WHERE
           code = ?2
           AND expires_at > ?3
           AND remaining_reads > 0`,
      )
      .bind(input.claimId, input.code, input.nowSeconds),
    db
      .prepare(
        `SELECT value, remaining_reads, expires_at
         FROM entries
         WHERE code = ?1 AND last_claim_id = ?2
         LIMIT 1`,
      )
      .bind(input.code, input.claimId),
  ]);

  if (!updateResult.success) {
    throw new Error("claim_update_failed");
  }

  const row = selectResult.results[0];

  if (row === undefined) {
    return null;
  }

  return {
    value: row.value,
    remainingReads: row.remaining_reads,
    expiresAtSeconds: row.expires_at,
  };
}

export async function cleanupEntries(
  db: D1Database,
  nowSeconds: number,
  limit: number,
): Promise<number> {
  const result = await db
    .prepare(
      `DELETE FROM entries
       WHERE code IN (
         SELECT code
         FROM entries
         WHERE expires_at <= ?1 OR remaining_reads <= 0
         LIMIT ?2
       )`,
    )
    .bind(nowSeconds, limit)
    .run();

  return result.meta.changes;
}
```

- [ ] **Step 4: Run repository tests and typecheck**

```bash
npm test -- test/repository.test.ts
npm run typecheck
```

Expected: all repository tests PASS.

- [ ] **Step 5: Commit Task 4**

```bash
git add src/repositories/entries.ts test/repository.test.ts
git commit -m "feat: add d1 entry repository"
```

---

### Task 5: Implement the Create Entry Endpoint

**Files:**
- Create: `src/routes/create-entry.ts`
- Modify: `src/app.ts`
- Test: `test/create-entry.test.ts`

**Interfaces:**
- Consumes: `generateCode`, `formatCode`, request limits, auth middleware, `createEntry`.
- Produces: `POST /v1/entries`.
- Response: JSON by default; plain code when `Accept: text/plain`.

- [ ] **Step 1: Write failing create endpoint tests**

Create `test/create-entry.test.ts`:

```ts
import { env, exports } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";

describe("POST /v1/entries", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("requires the create API token", async () => {
    const response = await exports.default.fetch(
      "https://example.test/v1/entries",
      {
        method: "POST",
        headers: {
          "Content-Type": "text/plain; charset=utf-8",
        },
        body: "ticket",
      },
    );

    expect(response.status).toBe(401);
  });

  it("creates a one-read, one-hour entry by default", async () => {
    const response = await exports.default.fetch(
      "https://example.test/v1/entries",
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${env.API_TOKEN}`,
          "Content-Type": "text/plain; charset=utf-8",
        },
        body: "ticket",
      },
    );

    expect(response.status).toBe(201);
    expect(response.headers.get("cache-control")).toBe("no-store");

    const body = await response.json<{
      code: string;
      expires_at: string;
      max_reads: number;
    }>();

    expect(body.code).toMatch(
      /^[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{4}-[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{4}$/,
    );
    expect(body.max_reads).toBe(1);
  });

  it("returns only the code when Accept is text/plain", async () => {
    const response = await exports.default.fetch(
      "https://example.test/v1/entries?ttl=60&reads=3",
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${env.API_TOKEN}`,
          "Content-Type": "text/plain; charset=utf-8",
          Accept: "text/plain",
        },
        body: "ticket",
      },
    );

    expect(response.status).toBe(201);
    expect(await response.text()).toMatch(/^[A-Z2-9]{4}-[A-Z2-9]{4}$/);
    expect(response.headers.get("x-rendezkey-max-reads")).toBe("3");
  });

  it("rejects an oversized payload", async () => {
    const response = await exports.default.fetch(
      "https://example.test/v1/entries",
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${env.API_TOKEN}`,
          "Content-Type": "text/plain; charset=utf-8",
        },
        body: "a".repeat(65_537),
      },
    );

    expect(response.status).toBe(413);
  });
});
```

- [ ] **Step 2: Run create tests and verify failure**

```bash
npm test -- test/create-entry.test.ts
```

Expected: FAIL because the route is not registered.

- [ ] **Step 3: Implement `src/routes/create-entry.ts`**

```ts
import type { Context } from "hono";
import { formatCode, generateCode } from "../domain/code";
import {
  parseMaxReads,
  parseTtl,
  readUtf8Body,
} from "../domain/limits";
import { problem } from "../http/errors";
import { createEntry } from "../repositories/entries";

const MAX_CODE_INSERT_ATTEMPTS = 5;

function epochSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

function isUniqueConstraintError(error: unknown): boolean {
  return (
    error instanceof Error &&
    error.message.toLowerCase().includes("unique")
  );
}

export async function createEntryRoute(
  c: Context<{ Bindings: CloudflareBindings }>,
): Promise<Response> {
  const contentType = c.req.header("Content-Type") ?? "";

  if (!contentType.toLowerCase().startsWith("text/plain")) {
    return problem(
      c,
      400,
      "invalid_request",
      "Invalid request",
      "Content-Type must be text/plain",
    );
  }

  let ttlSeconds: number;
  let maxReads: number;
  let value: string;

  try {
    ttlSeconds = parseTtl(c.req.query("ttl"));
    maxReads = parseMaxReads(c.req.query("reads"));
    value = await readUtf8Body(c.req.raw);
  } catch (error) {
    const code = error instanceof Error ? error.message : "invalid_request";

    if (code === "payload_too_large") {
      return problem(
        c,
        413,
        "payload_too_large",
        "Payload too large",
      );
    }

    return problem(
      c,
      400,
      "invalid_request",
      "Invalid request",
      code,
    );
  }

  const nowSeconds = epochSeconds();
  const expiresAtSeconds = nowSeconds + ttlSeconds;

  for (
    let attempt = 0;
    attempt < MAX_CODE_INSERT_ATTEMPTS;
    attempt += 1
  ) {
    const normalizedCode = generateCode();

    try {
      await createEntry(c.env.DB, {
        code: normalizedCode,
        value,
        nowSeconds,
        expiresAtSeconds,
        maxReads,
      });

      const displayCode = formatCode(normalizedCode);
      const expiresAt = new Date(expiresAtSeconds * 1000).toISOString();

      console.log(
        JSON.stringify({
          event: "entry_created",
          payload_bytes: new TextEncoder().encode(value).byteLength,
          ttl_seconds: ttlSeconds,
          max_reads: maxReads,
          status: 201,
        }),
      );

      if ((c.req.header("Accept") ?? "").includes("text/plain")) {
        c.header("Content-Type", "text/plain; charset=utf-8");
        c.header("X-RendezKey-Expires-At", expiresAt);
        c.header("X-RendezKey-Max-Reads", String(maxReads));
        return c.body(displayCode, 201);
      }

      return c.json(
        {
          code: displayCode,
          expires_at: expiresAt,
          max_reads: maxReads,
        },
        201,
      );
    } catch (error) {
      if (isUniqueConstraintError(error)) {
        continue;
      }

      throw error;
    }
  }

  return problem(
    c,
    503,
    "code_generation_failed",
    "Code generation failed",
  );
}
```

- [ ] **Step 4: Register the route and error handler in `src/app.ts`**

Replace `src/app.ts` with:

```ts
import { Hono } from "hono";
import { createEntryRoute } from "./routes/create-entry";
import { health } from "./routes/health";
import { requireCreateAuth } from "./http/auth";
import { problem } from "./http/errors";

export function createApp() {
  const app = new Hono<{ Bindings: CloudflareBindings }>();

  app.use("*", async (c, next) => {
    await next();
    c.header("Cache-Control", "no-store");
  });

  app.get("/healthz", health);

  app.post(
    "/v1/entries",
    requireCreateAuth,
    createEntryRoute,
  );

  app.notFound((c) =>
    problem(c, 404, "not_found", "Route not found"),
  );

  app.onError((error, c) => {
    console.error(
      JSON.stringify({
        event: "unhandled_error",
        message: error.message,
        status: 500,
      }),
    );

    return problem(c, 500, "internal_error", "Internal server error");
  });

  return app;
}
```

- [ ] **Step 5: Run create endpoint tests and typecheck**

```bash
npm test -- test/create-entry.test.ts
npm run typecheck
```

Expected: create endpoint tests PASS.

- [ ] **Step 6: Commit Task 5**

```bash
git add src/app.ts src/routes/create-entry.ts test/create-entry.test.ts
git commit -m "feat: add temporary string creation endpoint"
```

---

### Task 6: Implement the Atomic Claim Endpoint

**Files:**
- Create: `src/routes/claim-entry.ts`
- Modify: `src/app.ts`
- Test: `test/claim-entry.test.ts`
- Test: `test/concurrent-claim.test.ts`

**Interfaces:**
- Consumes: `normalizeCode`, `claimEntry`.
- Produces: `POST /v1/entries/:code/claim`.
- Returns raw `text/plain` payload.
- Atomically enforces `remaining_reads`.

- [ ] **Step 1: Write failing claim endpoint tests**

Create `test/claim-entry.test.ts`:

```ts
import { env, exports } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";

async function create(reads = 1): Promise<string> {
  const response = await exports.default.fetch(
    `https://example.test/v1/entries?reads=${reads}`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${env.API_TOKEN}`,
        "Content-Type": "text/plain; charset=utf-8",
        Accept: "text/plain",
      },
      body: "iroh-ticket",
    },
  );

  expect(response.status).toBe(201);
  return response.text();
}

describe("POST /v1/entries/:code/claim", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("returns the exact original string", async () => {
    const code = await create();

    const response = await exports.default.fetch(
      `https://example.test/v1/entries/${code}/claim`,
      { method: "POST" },
    );

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toContain("text/plain");
    expect(response.headers.get("x-rendezkey-remaining-reads")).toBe("0");
    expect(await response.text()).toBe("iroh-ticket");
  });

  it("allows exactly the configured number of claims", async () => {
    const code = await create(3);

    for (const expectedRemaining of [2, 1, 0]) {
      const response = await exports.default.fetch(
        `https://example.test/v1/entries/${code}/claim`,
        { method: "POST" },
      );

      expect(response.status).toBe(200);
      expect(
        response.headers.get("x-rendezkey-remaining-reads"),
      ).toBe(String(expectedRemaining));
    }

    const exhausted = await exports.default.fetch(
      `https://example.test/v1/entries/${code}/claim`,
      { method: "POST" },
    );

    expect(exhausted.status).toBe(404);
  });

  it("normalizes lowercase and missing hyphen", async () => {
    const code = await create();
    const compactLowercase = code.replace("-", "").toLowerCase();

    const response = await exports.default.fetch(
      `https://example.test/v1/entries/${compactLowercase}/claim`,
      { method: "POST" },
    );

    expect(response.status).toBe(200);
  });

  it("uses the same 404 for invalid and unavailable codes", async () => {
    const invalid = await exports.default.fetch(
      "https://example.test/v1/entries/0000-0000/claim",
      { method: "POST" },
    );

    const missing = await exports.default.fetch(
      "https://example.test/v1/entries/ABCDEFGH/claim",
      { method: "POST" },
    );

    expect(invalid.status).toBe(404);
    expect(missing.status).toBe(404);
    await expect(invalid.json()).resolves.toMatchObject({
      code: "entry_not_available",
    });
    await expect(missing.json()).resolves.toMatchObject({
      code: "entry_not_available",
    });
  });
});
```

- [ ] **Step 2: Write the failing concurrency test**

Create `test/concurrent-claim.test.ts`:

```ts
import { env, exports } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";

describe("concurrent claims", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("never succeeds more than max_reads", async () => {
    const createResponse = await exports.default.fetch(
      "https://example.test/v1/entries?reads=3",
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${env.API_TOKEN}`,
          "Content-Type": "text/plain; charset=utf-8",
          Accept: "text/plain",
        },
        body: "ticket",
      },
    );

    const code = await createResponse.text();

    const responses = await Promise.all(
      Array.from({ length: 10 }, () =>
        exports.default.fetch(
          `https://example.test/v1/entries/${code}/claim`,
          { method: "POST" },
        ),
      ),
    );

    const successCount = responses.filter(
      (response) => response.status === 200,
    ).length;

    expect(successCount).toBe(3);
  });
});
```

- [ ] **Step 3: Run claim tests and verify failure**

```bash
npm test -- test/claim-entry.test.ts test/concurrent-claim.test.ts
```

Expected: FAIL because claim route is not registered.

- [ ] **Step 4: Implement `src/routes/claim-entry.ts`**

```ts
import type { Context } from "hono";
import { normalizeCode } from "../domain/code";
import { problem } from "../http/errors";
import { claimEntry } from "../repositories/entries";

function epochSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

function unavailable(c: Context): Response {
  return problem(
    c,
    404,
    "entry_not_available",
    "Entry not available",
  );
}

export async function claimEntryRoute(
  c: Context<{ Bindings: CloudflareBindings }>,
): Promise<Response> {
  const normalizedCode = normalizeCode(c.req.param("code"));

  if (normalizedCode === null) {
    return unavailable(c);
  }

  const claimed = await claimEntry(c.env.DB, {
    code: normalizedCode,
    nowSeconds: epochSeconds(),
    claimId: crypto.randomUUID(),
  });

  if (claimed === null) {
    console.log(
      JSON.stringify({
        event: "entry_claim_unavailable",
        status: 404,
      }),
    );
    return unavailable(c);
  }

  const expiresAt = new Date(
    claimed.expiresAtSeconds * 1000,
  ).toISOString();

  console.log(
    JSON.stringify({
      event: "entry_claimed",
      remaining_reads: claimed.remainingReads,
      status: 200,
    }),
  );

  c.header("Content-Type", "text/plain; charset=utf-8");
  c.header(
    "X-RendezKey-Remaining-Reads",
    String(claimed.remainingReads),
  );
  c.header("X-RendezKey-Expires-At", expiresAt);

  return c.body(claimed.value, 200);
}
```

- [ ] **Step 5: Register the public claim route with exact create-route auth**

Add the import and use route-specific middleware in `src/app.ts`:

```ts
import { claimEntryRoute } from "./routes/claim-entry";

app.post("/v1/entries/:code/claim", claimEntryRoute);

app.post(
  "/v1/entries",
  requireCreateAuth,
  createEntryRoute,
);
```

Do not register `requireCreateAuth` with `app.use("/v1/entries", ...)`; the claim route must remain public and code-only.

- [ ] **Step 6: Run claim, concurrency, and full tests**

```bash
npm test -- test/claim-entry.test.ts test/concurrent-claim.test.ts
npm test
npm run typecheck
```

Expected: all tests PASS; exactly three of ten concurrent requests succeed for `reads=3`.

- [ ] **Step 7: Commit Task 6**

```bash
git add src/app.ts src/routes/claim-entry.ts \
  test/claim-entry.test.ts test/concurrent-claim.test.ts
git commit -m "feat: add atomic entry claims"
```

---

### Task 7: Implement Scheduled Cleanup

**Files:**
- Modify: `src/scheduled.ts`
- Test: `test/scheduled.test.ts`

**Interfaces:**
- Consumes: `cleanupEntries`.
- Produces: `runCleanup(env, scheduledTimeMs): Promise<void>`.
- Deletes at most 1,000 rows per scheduled execution.

- [ ] **Step 1: Write the failing scheduled cleanup test**

Create `test/scheduled.test.ts`:

```ts
import { env } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";
import { runCleanup } from "../src/scheduled";

describe("scheduled cleanup", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();

    await env.DB.batch([
      env.DB
        .prepare(
          `INSERT INTO entries
           (code, value, created_at, expires_at, max_reads, remaining_reads)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
        )
        .bind("ABCDEFGH", "expired", 1000, 1100, 1, 1),
      env.DB
        .prepare(
          `INSERT INTO entries
           (code, value, created_at, expires_at, max_reads, remaining_reads)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
        )
        .bind("JKLMNPQR", "exhausted", 1000, 5000, 1, 0),
      env.DB
        .prepare(
          `INSERT INTO entries
           (code, value, created_at, expires_at, max_reads, remaining_reads)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
        )
        .bind("STUVWXYZ", "active", 1000, 5000, 1, 1),
    ]);
  });

  it("deletes expired and exhausted entries only", async () => {
    await runCleanup(env, 2_000_000);

    const rows = await env.DB.prepare(
      "SELECT code FROM entries ORDER BY code",
    ).all<{ code: string }>();

    expect(rows.results).toEqual([{ code: "STUVWXYZ" }]);
  });
});
```

- [ ] **Step 2: Run the test and verify failure**

```bash
npm test -- test/scheduled.test.ts
```

Expected: FAIL because current `runCleanup` is empty.

- [ ] **Step 3: Implement `src/scheduled.ts`**

```ts
import { cleanupEntries } from "./repositories/entries";

const CLEANUP_BATCH_SIZE = 1000;

export async function runCleanup(
  env: CloudflareBindings,
  scheduledTimeMs: number,
): Promise<void> {
  const nowSeconds = Math.floor(scheduledTimeMs / 1000);
  const deleted = await cleanupEntries(
    env.DB,
    nowSeconds,
    CLEANUP_BATCH_SIZE,
  );

  console.log(
    JSON.stringify({
      event: "cleanup_completed",
      deleted_rows: deleted,
      status: "ok",
    }),
  );
}
```

- [ ] **Step 4: Run scheduled and full tests**

```bash
npm test -- test/scheduled.test.ts
npm test
npm run typecheck
```

Expected: all tests PASS.

- [ ] **Step 5: Test the scheduled handler locally**

Run:

```bash
npm run dev -- --test-scheduled
```

In another terminal:

```bash
curl -fsS http://localhost:8787/__scheduled
```

Expected: HTTP success and a structured `cleanup_completed` log.

- [ ] **Step 6: Commit Task 7**

```bash
git add src/scheduled.ts test/scheduled.test.ts
git commit -m "feat: clean expired rendezkey entries"
```

---

### Task 8: Add README, Curl Contract Tests, and CI

**Files:**
- Create: `README.md`
- Create: `scripts/smoke-test.sh`
- Create: `.github/workflows/ci.yml`
- Modify: `package.json`

**Interfaces:**
- Produces: documented deployment and usage workflow.
- Produces: a repeatable deployment smoke test.
- Produces: CI validation for types, tests, generated bindings, and deploy dry-run.

- [ ] **Step 1: Create `scripts/smoke-test.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail

: "${BASE_URL:?BASE_URL is required}"
: "${RENDEZKEY_TOKEN:?RENDEZKEY_TOKEN is required}"

VALUE="iroh-ticket-test-$(date +%s)"

CODE="$(
  curl -fsS -X POST \
    "${BASE_URL}/v1/entries?ttl=60&reads=1" \
    -H "Authorization: Bearer ${RENDEZKEY_TOKEN}" \
    -H "Content-Type: text/plain; charset=utf-8" \
    -H "Accept: text/plain" \
    --data-binary "${VALUE}"
)"

RESULT="$(
  curl -fsS -X POST \
    "${BASE_URL}/v1/entries/${CODE}/claim"
)"

if [[ "${RESULT}" != "${VALUE}" ]]; then
  echo "Claimed value does not match uploaded value" >&2
  exit 1
fi

SECOND_STATUS="$(
  curl -sS -o /dev/null -w "%{http_code}" \
    -X POST "${BASE_URL}/v1/entries/${CODE}/claim"
)"

if [[ "${SECOND_STATUS}" != "404" ]]; then
  echo "Expected second claim to return 404, got ${SECOND_STATUS}" >&2
  exit 1
fi

echo "RendezKey smoke test passed"
```

Run:

```bash
chmod +x scripts/smoke-test.sh
```

- [ ] **Step 2: Add the smoke script to `package.json`**

Add:

```json
{
  "scripts": {
    "smoke": "./scripts/smoke-test.sh"
  }
}
```

Preserve all existing scripts.

- [ ] **Step 3: Write `README.md`**

The README must contain:

````markdown
# RendezKey

RendezKey stores a temporary UTF-8 string and returns a short human-safe
code. The string expires automatically and can only be claimed a configured
number of times.

## Security boundary

RendezKey stores plaintext. Do not use it for passwords, private keys,
long-lived credentials, or sensitive production data.

## Local setup

```bash
cp .dev.vars.example .dev.vars
npm install
npx wrangler d1 migrations apply rendezkey --local
npm run types
npm run dev
```

## Store a string

```bash
CODE=$(curl -fsS -X POST \
  "http://localhost:8787/v1/entries?ttl=3600&reads=1" \
  -H "Authorization: Bearer $RENDEZKEY_TOKEN" \
  -H "Content-Type: text/plain; charset=utf-8" \
  -H "Accept: text/plain" \
  --data-binary "$IROH_TICKET")
```

## Claim a string

```bash
curl -fsS -X POST \
  "http://localhost:8787/v1/entries/$CODE/claim"
```

## Deploy

```bash
npx wrangler d1 create rendezkey
npx wrangler d1 migrations apply rendezkey --remote
npx wrangler secret put API_TOKEN
npm run types
npm test
npm run deploy:dry
npm run deploy
```
````

Also document all limits, response status codes, and the `reads`/`ttl`
query parameters from the PRD.

- [ ] **Step 4: Create `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm

      - run: npm ci
      - run: npm run types
      - run: git diff --exit-code worker-configuration.d.ts
      - run: npm run typecheck
      - run: npm test
      - run: npm run deploy:dry
```

- [ ] **Step 5: Run the complete local verification suite**

```bash
npm run types
git diff --exit-code worker-configuration.d.ts
npm run typecheck
npm test
npm run deploy:dry
```

Expected:

- generated types are current;
- no TypeScript errors;
- all tests pass;
- Wrangler dry-run succeeds.

- [ ] **Step 6: Commit Task 8**

```bash
git add README.md scripts/smoke-test.sh \
  .github/workflows/ci.yml package.json package-lock.json
git commit -m "docs: add rendezkey usage and ci"
```

---

### Task 9: Deploy and Verify Production

**Files:**
- Modify: `wrangler.jsonc` only if Wrangler updates the concrete D1 resource metadata.
- No source-code change expected unless a smoke test identifies a defect.

**Interfaces:**
- Produces: one production Worker.
- Produces: one production D1 database.
- Produces: a verified public claim endpoint and authenticated create endpoint.

- [ ] **Step 1: Confirm Cloudflare authentication and project state**

```bash
npx wrangler whoami
git status --short
```

Expected: Wrangler shows the intended Cloudflare account and Git has no unintended changes.

- [ ] **Step 2: Apply the migration to the remote D1 database**

```bash
npx wrangler d1 migrations apply rendezkey --remote
```

Expected: `0001_create_entries.sql` is applied successfully. If Task 1 already applied it remotely, Wrangler reports no pending migrations.

- [ ] **Step 3: Configure the production token interactively**

```bash
npx wrangler secret put API_TOKEN
```

Enter a long random token at the interactive prompt. Do not place the token in shell history, source files, or `wrangler.jsonc`.

- [ ] **Step 4: Run final pre-deploy validation**

```bash
npm run types
git diff --exit-code worker-configuration.d.ts
npm run typecheck
npm test
npm run deploy:dry
npx wrangler check startup
```

Expected: every command exits with code 0.

- [ ] **Step 5: Deploy**

```bash
npm run deploy
```

Expected: Wrangler prints the deployed `workers.dev` URL. Set `BASE_URL` in the next step to that exact URL, without a trailing slash.

- [ ] **Step 6: Run the production smoke test**

```bash
export BASE_URL="the exact URL printed by Wrangler"
export RENDEZKEY_TOKEN="the token entered in Step 3"
npm run smoke
unset RENDEZKEY_TOKEN
```

Expected: `RendezKey smoke test passed`.

- [ ] **Step 7: Verify logs do not expose sensitive values**

```bash
npx wrangler tail rendezkey --format json
```

While tailing, execute one create and one claim. Verify logs contain event names, status, payload size, TTL, and read counts, but do not contain payload content, the API token, the Authorization header, or the complete short code.

- [ ] **Step 8: Commit any Wrangler-generated resource metadata**

```bash
git add wrangler.jsonc worker-configuration.d.ts
git commit -m "ops: configure rendezkey production"
```

Skip the commit only when neither file changed.

---

## Final Verification Checklist

Run from a clean checkout:

```bash
npm ci
cp .dev.vars.example .dev.vars
npx wrangler d1 migrations apply rendezkey --local
npm run types
npm run typecheck
npm test
npm run deploy:dry
```

Then verify:

- [ ] `GET /healthz` returns 200.
- [ ] Missing create token returns 401.
- [ ] Correct create token returns an 8-character short code.
- [ ] Default claim succeeds once.
- [ ] Second default claim returns 404.
- [ ] `reads=3` succeeds exactly three times.
- [ ] Expired entries return 404.
- [ ] Concurrent claims never exceed `max_reads`.
- [ ] 64 KiB payload succeeds.
- [ ] 64 KiB + 1 byte returns 413.
- [ ] Cron cleanup removes expired and exhausted records.
- [ ] Responses have `Cache-Control: no-store`.
- [ ] Logs contain no sensitive payload or secrets.
- [ ] Production smoke test passes.

---

## Implementation Notes for the Agent

- Read the PRD before implementing this plan.
- Retrieve current Cloudflare docs before changing Wrangler config or Workers API signatures.
- The Cloudflare skills explicitly recommend:
  - current `compatibility_date`;
  - `wrangler.jsonc`;
  - `wrangler types`;
  - D1 bindings instead of REST calls;
  - Worker secrets instead of plaintext vars;
  - structured observability;
  - no module-level request state;
  - no floating Promises;
  - cryptographically secure random values.
- Keep scope strict. Do not add UI, accounts, encryption, KV, Durable Objects, or an SDK during MVP implementation.
- If `db.batch<T>()` generic typing differs in the installed Workers types, adapt only the type annotation; preserve the two-statement transactional claim algorithm and its tests.
- The concurrency test is a release blocker.
