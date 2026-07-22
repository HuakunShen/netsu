import { describe, expect, test } from "bun:test";

import { buildQuicMatrix } from "./run-quic-matrix";

describe("QUIC container matrix", () => {
  test("contains the exact correctness cells", () => {
    const cells = buildQuicMatrix();

    expect(
      cells.map(
        ({ profile, direction, parallel }) =>
          `${profile} ${direction} P${parallel}`,
      ),
    ).toEqual([
      "baseline upload P1",
      "baseline reverse P1",
      "baseline upload P4",
      "baseline reverse P4",
      "constrained upload P1",
      "lossy upload P1",
    ]);
  });

  test("uses unique names and bounded per-cell timeouts", () => {
    const cells = buildQuicMatrix();

    expect(new Set(cells.map((cell) => cell.name)).size).toBe(cells.length);
    for (const cell of cells) {
      expect(cell.timeoutMs).toBe((cell.durationSeconds + 25) * 1_000);
    }
  });
});
