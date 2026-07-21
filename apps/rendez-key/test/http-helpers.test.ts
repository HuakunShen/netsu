import { describe, expect, it } from "vitest";
import {
  MAX_PAYLOAD_BYTES,
  parseMaxReads,
  parseTtl,
  utf8ByteLength,
} from "../src/domain/limits";

describe("request limits", () => {
  it("uses defaults", () => {
    expect(parseTtl(undefined)).toBe(3600);
    expect(parseMaxReads(undefined)).toBe(1);
  });

  it("accepts inclusive boundaries", () => {
    expect(parseTtl("60")).toBe(60);
    expect(parseTtl("604800")).toBe(604800);
    expect(parseMaxReads("1")).toBe(1);
    expect(parseMaxReads("100")).toBe(100);
  });

  it("rejects invalid values", () => {
    expect(() => parseTtl("59")).toThrow("invalid_ttl");
    expect(() => parseTtl("1.5")).toThrow("invalid_ttl");
    expect(() => parseMaxReads("0")).toThrow("invalid_reads");
    expect(() => parseMaxReads("101")).toThrow("invalid_reads");
  });

  it("counts UTF-8 bytes instead of JavaScript characters", () => {
    expect(utf8ByteLength("a")).toBe(1);
    expect(utf8ByteLength("中")).toBe(3);
    expect(MAX_PAYLOAD_BYTES).toBe(65_536);
  });
});
