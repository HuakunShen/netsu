import { describe, expect, it } from "vitest";
import { Pacer, readUdpHeader, writeUdpHeader } from "../src/transport/udp.ts";

describe("udp packet header", () => {
  it("round-trips pcount and timestamp", () => {
    const buf = Buffer.alloc(64);
    const now = 1_700_000_123_456;
    writeUdpHeader(buf, 42, now);
    const h = readUdpHeader(buf);
    expect(h.pcount).toBe(42);
    expect(Math.abs(h.sentMs - now)).toBeLessThan(1); // µs truncation only
  });
});

describe("Pacer", () => {
  it("holds ~1Mbit/s: 25 x 5000-bit sends take ≥ ~100ms", async () => {
    const pacer = new Pacer(1_000_000);
    const start = Date.now();
    for (let i = 0; i < 25; i++) await pacer.gate(5000);
    // 125_000 bits at 1Mbit/s = 125ms ideal; allow generous lower bound
    expect(Date.now() - start).toBeGreaterThanOrEqual(90);
  });

  it("does not throttle below the rate", async () => {
    const pacer = new Pacer(1_000_000_000);
    const start = Date.now();
    for (let i = 0; i < 25; i++) await pacer.gate(5000);
    expect(Date.now() - start).toBeLessThan(50);
  });
});
