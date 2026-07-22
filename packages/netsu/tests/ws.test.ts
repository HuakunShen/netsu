import { describe, expect, it, vi } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { nextPort } from "./helpers.ts";

describe("netsu client vs netsu server (ws)", () => {
  for (const reverse of [false, true]) {
    for (const parallel of [1, 2]) {
      it(`reverse=${reverse} parallel=${parallel}`, async () => {
        const port = await nextPort();
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

  // Regression guard for the ws-reverse teardown livelock. On a fast host a
  // high-throughput reverse-mode receiver is flooded with inbound data events
  // that monopolize the event loop and starve the wall-clock duration timer
  // for many seconds (a single ~11.6s event-loop stall was measured). Because
  // the client is the receiver, a starved timer means it never writes
  // TEST_END, so the server — still sending until it sees TEST_END — hits its
  // own `time + 10s` read cap and aborts the whole test with SERVER_ERROR.
  // The client now also enforces the deadline on the receive path itself
  // (client.ts #onReverseBytes), which the runtime keeps servicing under that
  // load. This test reproduces the failure deterministically without needing
  // real saturation: it inflates ONLY the client's duration setTimeout so that
  // timer effectively never fires, then asserts the reverse test still ends
  // near its own 1s deadline (via the receive path) rather than at the
  // server's ~11s cap. Before the fix, runClient rejected with SERVER_ERROR.
  it("reverse mode ends via the receive path when the duration timer is starved", async () => {
    const duration = 1;
    const realSetTimeout = globalThis.setTimeout;
    let neutralized = false;
    const spy = vi.spyOn(globalThis, "setTimeout").mockImplementation(((
      fn: (...a: unknown[]) => void,
      ms?: number,
      ...args: unknown[]
    ) => {
      // The duration timer is the first (and only) setTimeout scheduled for
      // exactly duration*1000 ms: interval reporting uses setInterval, and
      // every control read / ws handshake uses a much larger delay. Push just
      // that one far past the test window so the receive-path deadline check
      // is the only thing left that can end the test.
      if (!neutralized && ms === duration * 1000) {
        neutralized = true;
        return realSetTimeout(fn, 60_000, ...args);
      }
      return realSetTimeout(fn, ms as number, ...args);
    }) as typeof setTimeout);

    const port = await nextPort();
    const server = await startServer({ port, transport: "ws" });
    try {
      const started = Date.now();
      const r = await runClient("127.0.0.1", { port, transport: "ws", duration, reverse: true });
      const elapsedMs = Date.now() - started;
      expect(neutralized).toBe(true); // the duration timer really was disabled
      expect(r.receivedBytes).toBeGreaterThan(0);
      // Ended at its own deadline via the receive path, well before the
      // server's `time + 10s` (~11s) safety cap would have aborted it.
      expect(elapsedMs).toBeLessThan(5000);
      expect(r.durationSeconds).toBeLessThan(3);
    } finally {
      spy.mockRestore();
      await server.close();
    }
  }, 20000);

  it("ws server also enforces the single-test lock", async () => {
    const port = await nextPort();
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
