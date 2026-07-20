import { describe, expect, it } from "vitest";
import { runClient } from "../src/client.ts";
import { startServer } from "../src/server.ts";
import { HAS_IPERF3, nextPort, runIperf3Client, spawnIperf3Server } from "./helpers.ts";

describe.skipIf(!HAS_IPERF3)("udp vs official iperf3", () => {
  it("netsu client → iperf3 server: packets counted, loss small on loopback", async () => {
    const port = nextPort();
    const kill = await spawnIperf3Server(port);
    try {
      const r = await runClient("127.0.0.1", { port, udp: true, duration: 2, bandwidth: 5_000_000 });
      expect(r.udpStats).toBeDefined();
      expect(r.udpStats!.packets).toBeGreaterThan(100);
      expect(r.udpStats!.lostPercent).toBeLessThan(10);
      // rate should be near 5 Mbit/s, not unpaced
      expect(r.sendBitsPerSecond).toBeGreaterThan(2_000_000);
      expect(r.sendBitsPerSecond).toBeLessThan(10_000_000);
    } finally {
      kill();
    }
  }, 15000);

  it("iperf3 -u client → netsu server", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client([
        "-c", "127.0.0.1", "-p", String(port), "-t", "2", "-u", "-b", "5M",
      ]);
      expect(code).toBe(0);
      const end = (json as { end: { sum: { packets: number; lost_percent: number } } }).end;
      expect(end.sum.packets).toBeGreaterThan(100);
      expect(end.sum.lost_percent).toBeLessThan(10);
    } finally {
      await server.close();
    }
  }, 15000);
});

describe("udp netsu ↔ netsu", () => {
  for (const reverse of [false, true]) {
    it(`reverse=${reverse}`, async () => {
      const port = nextPort();
      const server = await startServer({ port });
      try {
        const r = await runClient("127.0.0.1", {
          port, udp: true, duration: 2, reverse, bandwidth: 5_000_000,
        });
        expect(r.udpStats!.packets).toBeGreaterThan(100);
        expect(r.udpStats!.lostPercent).toBeLessThan(10);
      } finally {
        await server.close();
      }
    }, 15000);
  }
});
