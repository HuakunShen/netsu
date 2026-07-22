import { describe, expect, it } from "vitest";

import { nextPort } from "./helpers.ts";

describe("test port allocation", () => {
  it("does not recycle a tiny per-worker window during one test run", async () => {
    const ports = await Promise.all(
      Array.from({ length: 16 }, () => nextPort()),
    );

    expect(new Set(ports).size).toBe(ports.length);
  });
});
