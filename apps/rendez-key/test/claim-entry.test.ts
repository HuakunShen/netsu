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
