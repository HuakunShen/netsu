import * as v from "valibot";

export interface StreamResult {
  id: number;
  bytes: number;
  retransmits: number;
  jitter: number; // seconds
  errors: number; // UDP lost packets
  packets: number;
  startTime: number;
  endTime: number;
}

export interface EndResults {
  senderHasRetransmits: number;
  streams: StreamResult[];
}

/** EXCHANGE_RESULTS payload, field names from iperf3 send_results(). */
export function encodeResults(r: EndResults): Record<string, unknown> {
  return {
    cpu_util_total: 0,
    cpu_util_user: 0,
    cpu_util_system: 0,
    sender_has_retransmits: r.senderHasRetransmits,
    streams: r.streams.map((s) => ({
      id: s.id,
      bytes: s.bytes,
      retransmits: s.retransmits,
      jitter: s.jitter,
      errors: s.errors,
      omitted_errors: 0,
      packets: s.packets,
      omitted_packets: 0,
      start_time: s.startTime,
      end_time: s.endTime,
    })),
  };
}

const WireStream = v.looseObject({
  id: v.number(),
  bytes: v.number(),
  retransmits: v.optional(v.number(), -1),
  jitter: v.optional(v.number(), 0),
  errors: v.optional(v.number(), 0),
  packets: v.optional(v.number(), 0),
  start_time: v.optional(v.number(), 0),
  end_time: v.optional(v.number(), 0),
});

const WireResults = v.looseObject({
  sender_has_retransmits: v.optional(v.number(), -1),
  streams: v.array(WireStream),
});

export function decodeResults(value: unknown): EndResults {
  const r = v.parse(WireResults, value);
  return {
    senderHasRetransmits: r.sender_has_retransmits,
    streams: r.streams.map((s) => ({
      id: s.id,
      bytes: s.bytes,
      retransmits: s.retransmits,
      jitter: s.jitter,
      errors: s.errors,
      packets: s.packets,
      startTime: s.start_time,
      endTime: s.end_time,
    })),
  };
}
