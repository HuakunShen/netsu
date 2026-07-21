import { describe, expect, it } from "vitest";
import { IntervalMeter, JitterTracker, bitsPerSecond } from "../src/stats.ts";

describe("bitsPerSecond", () => {
  it("converts bytes over seconds to bits/s", () => {
    expect(bitsPerSecond(1_000_000, 8)).toBe(1_000_000);
    expect(bitsPerSecond(100, 0)).toBe(0);
  });
});

describe("IntervalMeter", () => {
  it("reports per-interval deltas and running total", () => {
    const m = new IntervalMeter(1000);
    m.add(500);
    m.add(500);
    const first = m.snap(2000); // 1s later
    expect(first).toEqual({ start: 0, end: 1, bytes: 1000, bitsPerSecond: 8000 });
    m.add(250);
    const second = m.snap(3000);
    expect(second.start).toBe(1);
    expect(second.bytes).toBe(250);
    expect(m.totalBytes).toBe(1250);
  });
});

describe("JitterTracker", () => {
  it("tracks loss and out-of-order from packet counts", () => {
    const t = new JitterTracker();
    t.onPacket(1, 0, 10);
    t.onPacket(2, 10, 20);
    t.onPacket(5, 40, 50); // 3,4 missing
    t.onPacket(4, 30, 55); // 4 arrives late: out of order, no longer lost
    expect(t.received).toBe(4);
    expect(t.maxSeq).toBe(5);
    expect(t.outOfOrder).toBe(1);
    expect(t.lost).toBe(1); // 5 expected, 4 received
  });

  it("computes RFC1889 jitter (hand-computed sequence)", () => {
    const t = new JitterTracker();
    // transit times: 10, 12, 9 → d = 2, 3
    t.onPacket(1, 0, 10);   // first packet: jitter stays 0
    t.onPacket(2, 100, 112); // d=|12-10|=2 → jitter = 0 + (2-0)/16 = 0.125
    t.onPacket(3, 200, 209); // d=|9-12|=3 → jitter = 0.125 + (3-0.125)/16 ≈ 0.3047
    expect(t.jitterMs).toBeCloseTo(0.3047, 3);
  });
});
