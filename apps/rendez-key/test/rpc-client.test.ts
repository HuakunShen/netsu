import { env, exports } from "cloudflare:workers";
import { describe, expect, it } from "vitest";
import { hc } from "hono/client";
import type { AppType } from "../src/client";

describe("hono/client RPC", () => {
  it("is fully typed and works against the real fetch handler", async () => {
    const client = hc<AppType>("https://example.test", {
      fetch: (...args: Parameters<typeof fetch>) => exports.default.fetch(...args),
    });

    const created = await client["v1"]["entries"].$post(
      { query: { reads: "3" } },
      {
        init: {
          body: "rpc-ticket",
          headers: {
            Authorization: `Bearer ${env.API_TOKEN}`,
            "Content-Type": "text/plain; charset=utf-8",
          },
        },
      },
    );

    expect(created.status).toBe(201);
    // The create route returns JSON or plain text at the same status code,
    // negotiated at runtime via the Accept header — Hono's RPC types can't
    // discriminate that, so the JSON shape needs an explicit assertion here.
    const body = (await created.json()) as { code: string };
    expect(body.code).toMatch(/^[A-Z2-9]{4}-[A-Z2-9]{4}$/);

    const claimed = await client["v1"]["entries"][":code"]["claim"].$post({
      param: { code: body.code },
    });

    expect(claimed.status).toBe(200);
    expect(await claimed.text()).toBe("rpc-ticket");
  });
});
