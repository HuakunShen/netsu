import { randomBytes } from "node:crypto";
import type { DataChannel } from "./channel.ts";

/** Mutable per-stream accounting shared by client and server. */
export interface StreamCounters {
  id: number;
  bytes: number;
  packets: number;
  jitter: number; // seconds
  errors: number;
}

export function makeCounters(id: number): StreamCounters {
  return { id, bytes: 0, packets: 0, jitter: 0, errors: 0 };
}

/**
 * Stream `id` values as iperf3 actually assigns them (`iperf_add_stream` in
 * iperf_api.c), NOT a plain 1..N sequence. The first stream gets id 1; every
 * subsequent stream's id is (number of streams already added) + 2 — a
 * historical quirk iperf3's own source comments acknowledge and preserve for
 * compatibility. For N streams this produces 1, 3, 4, 5, ..., N+1.
 *
 * This matters because EXCHANGE_RESULTS's per-stream `id` is never
 * negotiated on the wire — the server looks up each incoming stream result
 * by matching `id` against its own internally assigned ids (see
 * iperf_api.c's get_results: `SLIST_FOREACH(sp, ...) if (sp->id == sid)
 * break;`, erroring "stream has an invalid id" otherwise). Both sides only
 * agree because both independently run this same counting scheme, in the
 * same stream-creation order. `existingCount` is how many streams this peer
 * has already added before the one being assigned now.
 */
export function nextStreamId(existingCount: number): number {
  return existingCount === 0 ? 1 : existingCount + 2;
}

export function attachReceiver(
  channel: DataChannel,
  counters: StreamCounters,
  onBytes?: (n: number) => void,
): void {
  channel.onData((n) => {
    counters.bytes += n;
    onBytes?.(n);
  });
}

/** Minimum real time between macrotask yields in the send loop, in ms. */
const YIELD_INTERVAL_MS = 1;

/** Send random data (defeats link compression) until isRunning() is false. */
export async function startSender(
  channel: DataChannel,
  counters: StreamCounters,
  len: number,
  isRunning: () => boolean,
  onBytes?: (n: number) => void,
): Promise<void> {
  try {
    const chunk = randomBytes(len);
    let lastYield = performance.now();
    while (isRunning()) {
      await channel.write(chunk);
      counters.bytes += chunk.length;
      onBytes?.(chunk.length);
      // Yield to a real macrotask, not just the microtask queue — but only
      // once per YIELD_INTERVAL_MS of wall time, not on every write. When the
      // peer is a separate OS process (e.g. real iperf3) that drains the
      // kernel socket buffer on its own, channel.write()'s underlying
      // socket.write() can keep returning synchronously-resolved writes
      // (Node's "fast path") for a long stretch without ever hitting
      // backpressure. A while loop of already-resolved awaits only ever
      // schedules microtasks, which run to completion before the event
      // loop's timers/I/O phases get a turn — so without *some* yield, the
      // duration timer driving isRunning() (and interval reporting) can be
      // starved for many seconds past when the test should have ended.
      // Timers only need a turn every few milliseconds, so amortizing the
      // yield this way preserves timer liveness at negligible cost even at
      // small block sizes, where yielding on every write was measured with
      // this repo's own bench-sender.ts to cost roughly 3x throughput
      // (~5.0 Gbps amortized vs. ~1.6 Gbps yield-every-write, at len=1460;
      // a reviewer's fixed-sink measurement elsewhere saw a larger 7.7x
      // drop, 5.25 -> 0.68 Gbps — same direction, different harness).
      // performance.now() (not Date.now()) so a backward wall-clock step
      // (e.g. NTP correction) can't stall this gate and starve the end
      // timer for the same reason it exists to prevent.
      const now = performance.now();
      if (now - lastYield >= YIELD_INTERVAL_MS) {
        lastYield = now;
        await new Promise<void>((resolve) => setImmediate(resolve));
      }
    }
  } catch {
    // channel torn down at test end — expected
  }
}
