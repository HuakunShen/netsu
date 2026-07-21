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
