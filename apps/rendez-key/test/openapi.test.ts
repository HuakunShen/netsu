import { exports } from "cloudflare:workers";
import { describe, expect, it } from "vitest";

describe("GET /openapi.json", () => {
  it("serves a valid OpenAPI 3.1 document", async () => {
    const response = await exports.default.fetch(
      "https://example.test/openapi.json",
    );

    expect(response.status).toBe(200);
    const spec = await response.json<Record<string, unknown>>();
    expect(spec.openapi).toMatch(/^3\.1/);
    expect(spec.paths).toHaveProperty("/healthz");
    expect(spec.paths).toHaveProperty("/v1/entries");
    expect(spec.paths).toHaveProperty("/v1/entries/{code}/claim");
  });
});

describe("GET /docs", () => {
  it("serves the Scalar UI HTML page", async () => {
    const response = await exports.default.fetch(
      "https://example.test/docs",
    );

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toContain("text/html");
    const body = await response.text();
    expect(body.toLowerCase()).toContain("scalar");
  });
});
