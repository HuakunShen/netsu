import { describe, expect, it } from "vitest";
import {
  CODE_ALPHABET,
  formatCode,
  generateCode,
  normalizeCode,
} from "../src/domain/code";

describe("code domain", () => {
  it("generates eight characters from the human-safe alphabet", () => {
    for (let index = 0; index < 100; index += 1) {
      const code = generateCode();
      expect(code).toHaveLength(8);
      for (const character of code) {
        expect(CODE_ALPHABET).toContain(character);
      }
    }
  });

  it("formats normalized code as four-four", () => {
    expect(formatCode("7K3MQ9TX")).toBe("7K3M-Q9TX");
  });

  it("normalizes case, spaces, and hyphens", () => {
    expect(normalizeCode(" 7k3m-q9tx ")).toBe("7K3MQ9TX");
    expect(normalizeCode("7K3M Q9TX")).toBe("7K3MQ9TX");
  });

  it("rejects invalid length and alphabet characters", () => {
    expect(normalizeCode("ABC")).toBeNull();
    expect(normalizeCode("0000-0000")).toBeNull();
    expect(normalizeCode("IIII-IIII")).toBeNull();
  });
});
