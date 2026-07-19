import { expect, it } from "vitest";
import { VERSION } from "../src/index.ts";

it("exports version", () => {
  expect(VERSION).toBe("0.2.0");
});
