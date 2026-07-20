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
      // -l 1460 pins the UDP block size: without it iperf3 auto-selects the
      // loopback interface's MTU-sized block (16332 bytes on a 16384-MTU
      // loopback), which at 5 Mbit/s over 2s yields ~77 datagrams — below
      // this test's `packets > 100` floor no matter whose server answers,
      // since that's arithmetic, not a netsu defect. Pinning the block size
      // makes the packet count independent of the host's loopback MTU.
      const { code, json } = await runIperf3Client([
        "-c", "127.0.0.1", "-p", String(port), "-t", "2", "-u", "-b", "5M", "-l", "1460",
      ]);
      expect(code).toBe(0);
      const end = (json as { end: { sum: { packets: number; lost_percent: number } } }).end;
      expect(end.sum.packets).toBeGreaterThan(100);
      expect(end.sum.lost_percent).toBeLessThan(10);
    } finally {
      await server.close();
    }
  }, 15000);

  it("iperf3 -u -R client → netsu server: negotiated (unpinned) block size — Fix 1", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      // Deliberately NOT pinning -l here, unlike the test above: the whole
      // point is to exercise iperf3's own auto-selected block size (16332
      // bytes on this host's 16384-MTU loopback interface — iperf3 3.21's
      // real default, not a made-up number), which is exactly the case the
      // pinned test above cannot catch. In reverse mode netsu is the UDP
      // *sender* (server.ts's ServerSession#runUdpSender), which is the
      // side that actually has to emit a 16332-byte datagram — on a host
      // where that exceeds the OS/runtime's send ceiling (e.g. stock
      // macOS's net.inet.udp.maxdgram, 9216 by default), this used to fail
      // completely: every send errored, and the accumulated errors were
      // promoted to a fatal "data stream failed", producing a
      // SERVER_ERROR and zero bytes transferred. Fix 1(a)/(b) probe the
      // actual send capability and clamp to it instead of failing, and
      // treat a UDP send error as counted rather than fatal — so this must
      // now complete with a real, positive byte count instead of aborting.
      const { code, json } = await runIperf3Client([
        "-c", "127.0.0.1", "-p", String(port), "-t", "2", "-u", "-b", "5M", "-R",
      ]);
      expect(code).toBe(0);
      const end = (json as { end: { sum: { bytes: number; packets: number } } }).end;
      expect(end.sum.bytes).toBeGreaterThan(0);
      expect(end.sum.packets).toBeGreaterThan(0);
    } finally {
      await server.close();
    }
  }, 15000);

  it("iperf3 -u -P 4 client → netsu server: parallel UDP streams", async () => {
    const port = nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client([
        "-c", "127.0.0.1", "-p", String(port), "-t", "2", "-u", "-b", "5M", "-l", "1460", "-P", "4",
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
