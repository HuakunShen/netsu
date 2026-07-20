import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { nextPort } from "./helpers.ts";

describe("netsu client vs netsu server (tcp)", () => {
  for (const reverse of [false, true]) {
    for (const parallel of [1, 3]) {
      it(`reverse=${reverse} parallel=${parallel}`, async () => {
        const port = nextPort();
        const server = await startServer({ port });
        try {
          const r = await runClient("127.0.0.1", { port, duration: 1, reverse, parallel });
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

  it("serves a second test after the first finishes", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      await runClient("127.0.0.1", { port, duration: 1 });
      const again = await runClient("127.0.0.1", { port, duration: 1 });
      expect(again.sentBytes).toBeGreaterThan(0);
    } finally {
      await server.close();
    }
  }, 15000);

  it("rejects a concurrent client with ACCESS_DENIED", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const first = runClient("127.0.0.1", { port, duration: 2 });
      await new Promise((r) => setTimeout(r, 500));
      await expect(runClient("127.0.0.1", { port, duration: 1 })).rejects.toThrow(/busy/);
      await first;
    } finally {
      await server.close();
    }
  }, 15000);
});
