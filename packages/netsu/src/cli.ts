#!/usr/bin/env node
import { defineCommand, runMain } from "citty";
import { runClient, type TestResult } from "./client.ts";
import { formatBits, formatBytes, intervalLine, parseBandwidth } from "./format.ts";
import { startServer } from "./server.ts";
import type { IntervalReport } from "./stats.ts";

/** Extract a plain message from anything a library call might throw. */
function describeError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Parse a required non-negative/positive integer CLI argument.
 * Rejects floats, empty strings, and anything below `min` with a message
 * naming the flag — never lets NaN or a negative number reach the library.
 */
function parseIntArg(s: string, name: string, min: number): number {
  const trimmed = s.trim();
  if (trimmed === "") {
    // citty's underlying parser treats `-t -5` as the boolean flag `-t`
    // plus a stray `-5` token (it looks like its own flag), leaving
    // args.time === "" — so a negative number after a short flag lands
    // here rather than as a parseable string. `--flag=-5` sidesteps it.
    throw new Error(`invalid ${name}: missing value (use --${name}=-N for a negative number)`);
  }
  const v = Number.parseInt(trimmed, 10);
  if (!Number.isInteger(v) || String(v) !== trimmed || v < min) {
    throw new Error(`invalid ${name}: ${s}`);
  }
  return v;
}

function parsePort(s: string): number {
  const v = parseIntArg(s, "port", 1);
  if (v > 65535) throw new Error(`invalid port: ${s}`);
  return v;
}

/** Resolve once SIGINT or SIGTERM arrives, running `onSignal` exactly once. */
function waitForShutdown(onSignal: () => Promise<void>): Promise<void> {
  return new Promise((resolve) => {
    let handled = false;
    const shutdown = () => {
      if (handled) return;
      handled = true;
      process.off("SIGINT", shutdown);
      process.off("SIGTERM", shutdown);
      void onSignal().then(resolve, resolve);
    };
    process.once("SIGINT", shutdown);
    process.once("SIGTERM", shutdown);
  });
}

const serverCmd = defineCommand({
  meta: { name: "server", description: "Start a netsu speed test server" },
  args: {
    port: { type: "string", alias: "p", default: "5201", description: "server port to listen on" },
    ws: { type: "boolean", default: false, description: "WebSocket mode (netsu-only)" },
  },
  async run({ args }) {
    try {
      const port = parsePort(args.port);
      const transport = args.ws ? "ws" : "tcp";
      const server = await startServer({ port, transport });
      console.log(`netsu server listening on ${server.port} (${transport})`);
      // Keep running (the listening server itself holds the event loop
      // open) until Ctrl-C/SIGTERM, then release the port cleanly instead
      // of relying on the process being killed out from under it.
      await waitForShutdown(() => server.close());
    } catch (err) {
      console.error(`netsu server: ${describeError(err)}`);
      process.exitCode = 1;
    }
  },
});

const clientCmd = defineCommand({
  meta: { name: "client", description: "Run a speed test against a netsu/iperf3 server" },
  args: {
    host: { type: "positional", required: true, description: "server host" },
    port: { type: "string", alias: "p", default: "5201", description: "server port" },
    time: { type: "string", alias: "t", default: "10", description: "duration in seconds" },
    udp: { type: "boolean", alias: "u", default: false, description: "use UDP" },
    ws: { type: "boolean", default: false, description: "use WebSocket transport (netsu-only)" },
    parallel: { type: "string", alias: "P", default: "1", description: "number of parallel streams" },
    reverse: { type: "boolean", alias: "R", default: false, description: "server sends, client receives" },
    bandwidth: { type: "string", alias: "b", description: "target bandwidth, e.g. 5M (UDP pacing, bits/s)" },
    len: { type: "string", alias: "l", description: "read/write block size in bytes" },
    interval: { type: "string", alias: "i", default: "1", description: "seconds between periodic reports (0 disables)" },
    json: { type: "boolean", default: false, description: "output results as JSON" },
  },
  async run({ args }) {
    try {
      if (args.udp && args.ws) throw new Error("--udp and --ws are mutually exclusive");

      const port = parsePort(args.port);
      const duration = parseIntArg(args.time, "time", 1);
      const parallel = parseIntArg(args.parallel, "parallel", 1);
      const intervalSec = parseIntArg(args.interval, "interval", 0);
      const len = args.len !== undefined ? parseIntArg(args.len, "len", 1) : undefined;
      const bandwidth = args.bandwidth !== undefined ? parseBandwidth(args.bandwidth) : undefined;

      const intervals: IntervalReport[] = [];
      const result = await runClient(args.host, {
        port,
        duration,
        parallel,
        udp: args.udp,
        transport: args.ws ? "ws" : "tcp",
        reverse: args.reverse,
        bandwidth,
        len,
        interval: intervalSec,
        onInterval: (r) => {
          intervals.push(r);
          // --json must emit nothing but the final JSON blob on stdout.
          if (!args.json) console.log(intervalLine(r));
        },
      });

      if (args.json) {
        console.log(JSON.stringify(toJson(result, intervals)));
      } else {
        printSummary(result);
      }
    } catch (err) {
      // Surface the library's phase-tagged message (e.g. "server busy
      // (ACCESS_DENIED)") rather than a raw stack trace, and always on
      // stderr so --json's stdout contract holds even on failure.
      console.error(`netsu client: ${describeError(err)}`);
      process.exitCode = 1;
    }
  },
});

function printSummary(r: TestResult): void {
  const dur = r.durationSeconds.toFixed(2);
  console.log("- - - - - - - - - - - - - - - - - - - - - - - - -");
  console.log(
    `[SUM]   0.00-${dur} sec  ${formatBytes(r.sentBytes).padStart(12)}  ${formatBits(r.sendBitsPerSecond).padStart(14)}  sender`,
  );
  console.log(
    `[SUM]   0.00-${dur} sec  ${formatBytes(r.receivedBytes).padStart(12)}  ${formatBits(r.receiveBitsPerSecond).padStart(14)}  receiver`,
  );
  if (r.udpStats) {
    const u = r.udpStats;
    console.log(
      `[SUM] jitter ${u.jitterMs.toFixed(3)} ms, lost ${u.lost}/${u.packets} (${u.lostPercent.toFixed(2)}%)`,
    );
  }
}

function toJson(r: TestResult, intervals: IntervalReport[]): Record<string, unknown> {
  return {
    start: {
      version: "netsu-0.2.0",
      test_start: {
        protocol: r.udp ? "UDP" : "TCP",
        reverse: r.reverse ? 1 : 0,
      },
    },
    intervals: intervals.map((i) => ({
      sum: { start: i.start, end: i.end, bytes: i.bytes, bits_per_second: i.bitsPerSecond },
    })),
    end: {
      sum_sent: { bytes: r.sentBytes, bits_per_second: r.sendBitsPerSecond, seconds: r.durationSeconds },
      sum_received: {
        bytes: r.receivedBytes,
        bits_per_second: r.receiveBitsPerSecond,
        seconds: r.durationSeconds,
      },
      ...(r.udpStats
        ? {
            sum: {
              jitter_ms: r.udpStats.jitterMs,
              lost_packets: r.udpStats.lost,
              packets: r.udpStats.packets,
              lost_percent: r.udpStats.lostPercent,
            },
          }
        : {}),
    },
  };
}

await runMain(
  defineCommand({
    meta: { name: "netsu", description: "iperf3-compatible network speed test" },
    subCommands: { server: serverCmd, client: clientCmd },
  }),
);
