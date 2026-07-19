import { describe, expect, it } from "vitest";
import { decodeParams, encodeParams, type TestParams } from "../src/protocol/params.ts";
import { decodeResults, encodeResults, type EndResults } from "../src/protocol/results.ts";

const params: TestParams = {
  udp: false, time: 10, parallel: 2, len: 131072, reverse: true, bandwidth: 0,
};

describe("params codec", () => {
  it("encodes iperf3 field names", () => {
    const j = encodeParams(params);
    expect(j).toMatchObject({ tcp: true, time: 10, parallel: 2, len: 131072, reverse: true });
    expect(j).not.toHaveProperty("udp");
    expect(j).not.toHaveProperty("bandwidth"); // tcp: no pacing field
    expect(j).toHaveProperty("client_version");
  });

  it("encodes udp with bandwidth, without reverse when false", () => {
    const j = encodeParams({ ...params, udp: true, reverse: false, bandwidth: 1048576, len: 1460 });
    expect(j).toMatchObject({ udp: true, bandwidth: 1048576, len: 1460 });
    expect(j).not.toHaveProperty("tcp");
    expect(j).not.toHaveProperty("reverse");
  });

  it("decodes its own output and tolerates unknown fields", () => {
    const decoded = decodeParams({ ...encodeParams(params), MSS: 1400, congestion: "cubic" });
    expect(decoded).toEqual(params);
  });

  it("rejects out-of-bounds values", () => {
    expect(() => decodeParams({ tcp: true, time: 10, parallel: 500, len: 1000 })).toThrow();
    expect(() => decodeParams({ tcp: true, time: 10, parallel: 1, len: 99999999 })).toThrow();
    expect(() => decodeParams({ time: 10 })).toThrow(); // neither tcp nor udp
  });
});

describe("results codec", () => {
  const results: EndResults = {
    senderHasRetransmits: -1,
    streams: [
      { id: 1, bytes: 5000, retransmits: -1, jitter: 0.002, errors: 3, packets: 100, startTime: 0, endTime: 10.01 },
    ],
  };

  it("round-trips through iperf3 field names", () => {
    const j = encodeResults(results);
    expect(j).toMatchObject({ cpu_util_total: 0, sender_has_retransmits: -1 });
    const s = (j as { streams: Record<string, unknown>[] }).streams[0]!;
    expect(s).toMatchObject({ id: 1, bytes: 5000, jitter: 0.002, errors: 3, packets: 100, start_time: 0, end_time: 10.01 });
    expect(decodeResults(j)).toEqual(results);
  });
});
