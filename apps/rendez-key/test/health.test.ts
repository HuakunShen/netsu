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
