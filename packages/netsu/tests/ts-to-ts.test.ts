import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { nextPort } from "./helpers.ts";

describe("netsu client vs netsu server (tcp)", () => {
  for (const reverse of [false, true]) {
    for (const parallel of [1, 3]) {
      it(`reverse=${reverse} parallel=${parallel}`, async () => {
        const port = await nextPort();
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
    const port = await nextPort();
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
    const port = await nextPort();
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

  // Fix 6: without this cap, a peer requesting e.g. {"time": 86400} (the
  // wire-level max protocol/params.ts's own bound still allows) would hold
  // this single-test lock — only one ServerSession runs at a time — for a
  // full day. maxTestSeconds rejects an over-long request up front, at
  // PARAM_EXCHANGE, rather than accepting the lock and only capping the
  // TEST_END wait later.
  it("rejects a requested time exceeding the server's maxTestSeconds", async () => {
    const port = await nextPort();
    const server = await startServer({ port, maxTestSeconds: 5 });
    try {
      await expect(runClient("127.0.0.1", { port, duration: 10 })).rejects.toThrow(/SERVER_ERROR/);
    } finally {
      await server.close();
    }
  }, 15000);
});
