import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { HAS_IPERF3, nextPort, spawnIperf3Server } from "./helpers.ts";

describe.skipIf(!HAS_IPERF3)("netsu client vs official iperf3 server (tcp)", () => {
  it("upload: transfers and exchanges results", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, duration: 2 });
      expect(r.reverse).toBe(false);
      expect(r.sentBytes).toBeGreaterThan(1_000_000); // loopback: way more in 2s
      expect(r.receivedBytes).toBeGreaterThan(0);
      expect(r.receivedBytes).toBeLessThanOrEqual(r.sentBytes * 1.01);
      expect(r.sendBitsPerSecond).toBeGreaterThan(1_000_000);
    } finally {
      kill();
    }
  }, 15000);

  it("reverse (-R): receives from server", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, duration: 2, reverse: true });
      expect(r.receivedBytes).toBeGreaterThan(1_000_000);
      expect(r.local.senderHasRetransmits).toBe(-1); // we are the receiver
    } finally {
      kill();
    }
  }, 15000);

  it("parallel (-P 3): three streams with per-stream results", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, duration: 2, parallel: 3 });
      expect(r.local.streams).toHaveLength(3);
      expect(r.remote.streams).toHaveLength(3);
      for (const s of r.local.streams) expect(s.bytes).toBeGreaterThan(0);
    } finally {
      kill();
    }
  }, 15000);

  it("reports intervals roughly every second", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const reports: number[] = [];
      await runClient("127.0.0.1", {
        port, duration: 3,
        onInterval: (rep) => reports.push(rep.bitsPerSecond),
      });
      expect(reports.length).toBeGreaterThanOrEqual(2);
      for (const bps of reports) expect(bps).toBeGreaterThan(0);
    } finally {
      kill();
    }
  }, 15000);
});
