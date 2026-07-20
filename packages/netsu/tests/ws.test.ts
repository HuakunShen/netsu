import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { nextPort } from "./helpers.ts";

describe("netsu client vs netsu server (ws)", () => {
  for (const reverse of [false, true]) {
    for (const parallel of [1, 2]) {
      it(`reverse=${reverse} parallel=${parallel}`, async () => {
        const port = nextPort();
        const server = await startServer({ port, transport: "ws" });
        try {
          const r = await runClient("127.0.0.1", {
            port, transport: "ws", duration: 1, reverse, parallel,
          });
          expect(r.sentBytes).toBeGreaterThan(100_000);
          expect(r.receivedBytes).toBeGreaterThan(0);
          expect(r.receivedBytes).toBeLessThanOrEqual(r.sentBytes * 1.01);
          expect(r.local.streams).toHaveLength(parallel);
        } finally {
          await server.close();
        }
      }, 15000);
    }
  }

  it("ws server also enforces the single-test lock", async () => {
    const port = nextPort();
    const server = await startServer({ port, transport: "ws" });
    try {
      const first = runClient("127.0.0.1", { port, transport: "ws", duration: 2 });
      await new Promise((r) => setTimeout(r, 500));
      await expect(
        runClient("127.0.0.1", { port, transport: "ws", duration: 1 }),
      ).rejects.toThrow(/busy/);
      await first;
    } finally {
      await server.close();
    }
  }, 15000);
});
