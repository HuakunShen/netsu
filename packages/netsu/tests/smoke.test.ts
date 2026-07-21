import { expect, it } from "vitest";
import { VERSION } from "../src/index.ts";
import pkg from "../package.json" with { type: "json" };

// Assert against package.json (the same single source of truth VERSION
// itself reads from — see src/version.ts) rather than a hardcoded string:
// a hardcoded "0.2.0" here would fail on the very next version bump for a
// reason that has nothing to do with an actual regression.
it("exports version", () => {
  expect(VERSION).toBe(pkg.version);
});
