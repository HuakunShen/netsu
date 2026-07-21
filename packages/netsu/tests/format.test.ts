import { describe, expect, it } from "vitest";
import { formatBits, formatBytes, intervalLine, parseBandwidth } from "../src/format.ts";

describe("parseBandwidth", () => {
  it("parses plain numbers and K/M/G suffixes (bits/s)", () => {
    expect(parseBandwidth("1000")).toBe(1000);
    expect(parseBandwidth("5M")).toBe(5_000_000);
    expect(parseBandwidth("2.5m")).toBe(2_500_000);
    expect(parseBandwidth("1G")).toBe(1_000_000_000);
    expect(() => parseBandwidth("fast")).toThrow();
  });
});

describe("formatters", () => {
  it("formats byte and bit magnitudes", () => {
    expect(formatBytes(117_440_512)).toBe("112 MBytes");
    expect(formatBytes(512)).toBe("512 Bytes");
    expect(formatBits(943_000_000)).toBe("943 Mbits/sec");
    expect(formatBits(1_500)).toBe("1.50 Kbits/sec");
  });

  it("renders an interval line", () => {
    const line = intervalLine({ start: 0, end: 1, bytes: 117_440_512, bitsPerSecond: 943_000_000 });
    expect(line).toContain("0.00-1.00");
    expect(line).toContain("112 MBytes");
    expect(line).toContain("943 Mbits/sec");
  });
});
