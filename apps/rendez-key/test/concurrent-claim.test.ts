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
