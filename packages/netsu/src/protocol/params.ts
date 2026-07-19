import * as v from "valibot";

export const DEFAULT_TCP_LEN = 131072;
export const DEFAULT_UDP_LEN = 1460;
export const DEFAULT_UDP_BANDWIDTH = 1048576; // 1 Mbit/s, iperf3's UDP default
export const MAX_PARALLEL = 128;
export const MAX_LEN = 1048576;

export interface TestParams {
  udp: boolean;
  time: number;
  parallel: number;
  len: number;
  reverse: boolean;
  bandwidth: number; // bits/s; 0 = unpaced (TCP)
}

/** PARAM_EXCHANGE payload, field names from iperf3 send_parameters(). */
export function encodeParams(p: TestParams): Record<string, unknown> {
  return {
    ...(p.udp ? { udp: true } : { tcp: true }),
    omit: 0,
    time: p.time,
    num: 0,
    blockcount: 0,
    parallel: p.parallel,
    ...(p.reverse ? { reverse: true } : {}),
    len: p.len,
    ...(p.udp ? { bandwidth: p.bandwidth } : {}),
    pacing_timer: 1000,
    client_version: "netsu-0.2.0",
  };
}

const WireParams = v.looseObject({
  tcp: v.optional(v.boolean()),
  udp: v.optional(v.boolean()),
  time: v.pipe(v.number(), v.minValue(1), v.maxValue(86400)),
  parallel: v.pipe(v.number(), v.integer(), v.minValue(1), v.maxValue(MAX_PARALLEL)),
  reverse: v.optional(v.boolean()),
  len: v.pipe(v.number(), v.integer(), v.minValue(4), v.maxValue(MAX_LEN)),
  bandwidth: v.optional(v.pipe(v.number(), v.minValue(0))),
});

export function decodeParams(value: unknown): TestParams {
  const p = v.parse(WireParams, value);
  if (!p.tcp && !p.udp) throw new Error("params: neither tcp nor udp");
  return {
    udp: p.udp === true,
    time: p.time,
    parallel: p.parallel,
    len: p.len,
    reverse: p.reverse === true,
    bandwidth: p.bandwidth ?? 0,
  };
}
