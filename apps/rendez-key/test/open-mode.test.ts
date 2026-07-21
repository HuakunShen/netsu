import { env } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";
import { createApp } from "../src/app";

const { app } = createApp();

type FakeLimiter = {
  limit: (options: { key: string }) => Promise<{ success: boolean }>;
};

const ALLOW: FakeLimiter = { limit: async () => ({ success: true }) };
const DENY: FakeLimiter = { limit: async () => ({ success: false }) };

/**
 * Build a bindings object for `app.request(...)`. Cast through `unknown` so we
 * can vary `PUBLIC_CREATE` (generated as the literal `"false"`) and inject a
 * plain-object rate limiter without fighting the generated types.
 */
function makeEnv(opts: {
  publicCreate?: string;
  apiToken?: string;
  limiter?: FakeLimiter;
}): CloudflareBindings {
  return {
    DB: env.DB,
    PUBLIC_CREATE: opts.publicCreate ?? "false",
    API_TOKEN: opts.apiToken ?? "test-token",
    CREATE_LIMITER: opts.limiter ?? ALLOW,
  } as unknown as CloudflareBindings;
}

async function post(
  path: string,
  bindings: CloudflareBindings,
  init: RequestInit = {},
): Promise<Response> {
  return app.request(
    `https://example.test${path}`,
    {
      method: "POST",
      headers: {
        "Content-Type": "text/plain; charset=utf-8",
        ...(init.headers ?? {}),
      },
      body: init.body ?? "ticket",
    },
    bindings,
  );
}

describe("POST /v1/entries — open (anonymous) tier", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("rejects anonymous creates when open mode is off", async () => {
    const response = await post("/v1/entries", makeEnv({ publicCreate: "false" }));
    expect(response.status).toBe(401);
  });

  it("accepts an anonymous create in open mode with default caps", async () => {
    const response = await post("/v1/entries", makeEnv({ publicCreate: "true" }));

    expect(response.status).toBe(201);
    const body = await response.json<{ max_reads: number }>();
    expect(body.max_reads).toBe(1);
  });

  it("accepts anonymous requests at the tight-tier ceilings", async () => {
    const response = await post(
      "/v1/entries?ttl=3600&reads=5",
      makeEnv({ publicCreate: "true" }),
    );
    expect(response.status).toBe(201);
  });

  it("rejects a TTL above the anonymous 1-hour ceiling", async () => {
    const response = await post(
      "/v1/entries?ttl=7200",
      makeEnv({ publicCreate: "true" }),
    );
    expect(response.status).toBe(400);
  });

  it("rejects reads above the anonymous ceiling of 5", async () => {
    const response = await post(
      "/v1/entries?reads=6",
      makeEnv({ publicCreate: "true" }),
    );
    expect(response.status).toBe(400);
  });

  it("rejects a payload above the anonymous 8 KiB ceiling", async () => {
    const response = await post("/v1/entries", makeEnv({ publicCreate: "true" }), {
      body: "a".repeat(8_193),
    });
    expect(response.status).toBe(413);
  });

  it("returns 429 when the per-IP rate limit is exceeded", async () => {
    const response = await post(
      "/v1/entries",
      makeEnv({ publicCreate: "true", limiter: DENY }),
    );
    expect(response.status).toBe(429);
  });
});

describe("POST /v1/entries — privileged tier alongside open mode", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("lets a valid token unlock full caps and bypass the rate limit", async () => {
    // Open mode on AND a denying limiter: a valid token must still succeed with
    // privileged caps (ttl 7200 > 1h, reads 100 > 5), proving the tiers coexist.
    const response = await post(
      "/v1/entries?ttl=7200&reads=100",
      makeEnv({ publicCreate: "true", apiToken: "secret", limiter: DENY }),
      { headers: { Authorization: "Bearer secret" } },
    );

    expect(response.status).toBe(201);
    const body = await response.json<{ max_reads: number }>();
    expect(body.max_reads).toBe(100);
  });

  it("rejects an invalid token instead of downgrading to anonymous", async () => {
    const response = await post(
      "/v1/entries",
      makeEnv({ publicCreate: "true", apiToken: "secret" }),
      { headers: { Authorization: "Bearer wrong" } },
    );
    expect(response.status).toBe(401);
  });

  it("rejects a token request when no API token is configured", async () => {
    const response = await post(
      "/v1/entries",
      makeEnv({ publicCreate: "true", apiToken: "" }),
      { headers: { Authorization: "Bearer anything" } },
    );
    expect(response.status).toBe(401);
  });
});
