import { describe, expect, it } from "vitest";
import { nextStreamId } from "../src/streams/runner.ts";

describe("nextStreamId", () => {
  // Fix 7: iperf3's own stream-id assignment quirk (see the doc comment on
  // nextStreamId) — 1, then +2 per additional stream, NOT a plain 1..N
  // sequence. Only iperf3 rejecting a mismatched id caught a regression
  // here before; that's an indirect, interop-only signal a future
  // (e.g. Rust) port is unlikely to have running early, so it's the detail
  // most likely to get silently reintroduced wrong.
  it("assigns iperf3's 1, 3, 4, 5, ... sequence, not a plain count", () => {
    const ids: number[] = [];
    for (let existing = 0; existing < 5; existing++) {
      ids.push(nextStreamId(existing));
    }
    expect(ids).toEqual([1, 3, 4, 5, 6]);
  });

  it("first stream always gets id 1", () => {
    expect(nextStreamId(0)).toBe(1);
  });
});
