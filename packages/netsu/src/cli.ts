#!/usr/bin/env node
import { defineCommand, renderUsage, runMain } from "citty";
import { runClient, type TestResult } from "./client.ts";
import { formatBits, formatBytes, intervalLine, parseBandwidth, parseByteSize } from "./format.ts";
import { startServer } from "./server.ts";
import type { IntervalReport } from "./stats.ts";

/** Extract a plain message from anything a library call might throw. */
function describeError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Guard against mri's "a repeated flag collapses into an array" behavior
 * before any string-specific parsing (`.trim()`, regexes, ...) runs on it.
 * citty's compile-time types promise every `type: "string"` arg is a
 * `string`, but at runtime `-p 1 -p 2` makes `args.port` a `string[]` — with
 * no guard, `.trim()` throws a raw "s.trim is not a function" TypeError that
 * leaks an internal detail instead of a clean CLI error message.
 */
function requireSingleValue(raw: unknown, name: string): string {
  if (typeof raw !== "string") {
    if (Array.isArray(raw)) throw new Error(`invalid ${name}: specified more than once`);
    throw new Error(`invalid ${name}: ${String(raw)}`);
  }
  return raw;
}

/**
 * Parse a required non-negative/positive integer CLI argument.
 * Rejects floats, empty strings, and anything below `min` with a message
 * naming the flag — never lets NaN or a negative number reach the library.
 *
 * `negativeHint`, when set, is appended to the "missing value" message.
 * citty's parser collapses two different situations to the same `""`: the
 * flag being the last token on the line (value truly absent) and the flag
 * being immediately followed by a token that itself looks like a flag, e.g.
 * `-t -5` or `-t -p` (mri treats the following `-5`/`-p` as its own token
 * rather than `-t`'s value). Only the latter case benefits from the
 * `--flag=-N` negative-number hint, so callers compute `negativeHint` from
 * the raw argv (see `missingValueLooksLikeFlag`) rather than us guessing here.
 */
function parseIntArg(raw: unknown, name: string, min: number, negativeHint = ""): number {
  const s = requireSingleValue(raw, name);
  const trimmed = s.trim();
  if (trimmed === "") {
    throw new Error(`invalid ${name}: missing value${negativeHint}`);
  }
  const v = Number.parseInt(trimmed, 10);
  if (!Number.isInteger(v) || String(v) !== trimmed || v < min) {
    throw new Error(`invalid ${name}: ${s}`);
  }
  return v;
}

/**
 * True when one of `flags` (e.g. `-t`, `--time`) appears in `rawArgs`
 * immediately followed by a token that itself starts with `-` (and isn't the
 * `--` end-of-options marker) — i.e. the value looked like a flag rather
 * than being altogether absent. Returns false both when the flag is missing
 * entirely and when it's the last token on the line, so those cases get the
 * plain "missing value" message instead of the negative-number hint.
 */
function missingValueLooksLikeFlag(rawArgs: string[], flags: string[]): boolean {
  const idx = rawArgs.findIndex((a) => flags.includes(a));
  if (idx === -1) return false;
  const next = rawArgs[idx + 1];
  return next !== undefined && next !== "--" && next.startsWith("-");
}

function negativeHintFor(name: string, rawArgs: string[], flags: string[]): string {
  return missingValueLooksLikeFlag(rawArgs, flags)
    ? ` (use --${name}=-N for a negative number)`
    : "";
}

function parsePort(raw: unknown, rawArgs: string[]): number {
  const v = parseIntArg(raw, "port", 1, negativeHintFor("port", rawArgs, ["-p", "--port"]));
  if (v > 65535) throw new Error(`invalid port: ${String(raw)}`);
  return v;
}

/**
 * Reject an unrecognized flag rather than silently ignoring it — citty's
 * parser (mri underneath) isn't run in strict mode, so a typo like
 * `--revese` for `--reverse` would otherwise be parsed as a harmless unknown
 * key and the test would silently run in the wrong direction. Only scans
 * for flag-shaped tokens (leading `-`); positionals (the host) are untouched.
 *
 * Tokens immediately after a known *string*-type flag are never flagged as
 * unknown, even if they themselves look like a flag: mri attempts to use
 * that token as the preceding flag's value (see `missingValueLooksLikeFlag`),
 * so e.g. the stray `-5` in `-t -5` is that attempted (and separately
 * rejected, with a negative-number hint) value for `-t`, not an unrelated
 * unknown flag — flagging it here too would shadow that more useful message.
 */
function findUnknownFlag(rawArgs: string[], argsDef: Record<string, { type?: string; alias?: string | string[] }>): string | undefined {
  const known = new Set<string>(["--help", "-h", "--version"]);
  const stringFlagTokens = new Set<string>();
  for (const [key, def] of Object.entries(argsDef)) {
    if (def.type === "positional") continue;
    known.add(`--${key}`);
    if (def.type === "boolean") known.add(`--no-${key}`);
    const aliases = Array.isArray(def.alias) ? def.alias : def.alias ? [def.alias] : [];
    const tokens = aliases.map((a) => (a.length === 1 ? `-${a}` : `--${a}`)).concat(`--${key}`);
    for (const t of tokens) known.add(t);
    if (def.type === "string") for (const t of tokens) stringFlagTokens.add(t);
  }
  for (let i = 0; i < rawArgs.length; i++) {
    const token = rawArgs[i] as string;
    if (!token.startsWith("-") || token === "-" || token === "--") continue;
    if (i > 0 && stringFlagTokens.has(rawArgs[i - 1] as string)) continue;
    const name = token.split("=")[0] as string;
    if (!known.has(name)) return token;
  }
  return undefined;
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

const serverArgsDef = {
  port: { type: "string", alias: "p", default: "5201", description: "server port to listen on" },
  ws: { type: "boolean", default: false, description: "WebSocket mode (netsu-only)" },
} as const;

const serverCmd = defineCommand({
  meta: { name: "server", description: "Start a netsu speed test server" },
  args: serverArgsDef,
  async run({ args, rawArgs }) {
    try {
      const unknown = findUnknownFlag(rawArgs, serverArgsDef);
      if (unknown) throw new Error(`unknown option: ${unknown}`);

      const port = parsePort(args.port, rawArgs);
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

const clientArgsDef = {
  host: { type: "positional", required: true, description: "server host" },
  port: { type: "string", alias: "p", default: "5201", description: "server port" },
  time: { type: "string", alias: "t", default: "10", description: "duration in seconds" },
  udp: { type: "boolean", alias: "u", default: false, description: "use UDP" },
  ws: { type: "boolean", default: false, description: "use WebSocket transport (netsu-only)" },
  parallel: { type: "string", alias: "P", default: "1", description: "number of parallel streams" },
  reverse: { type: "boolean", alias: "R", default: false, description: "server sends, client receives" },
  bandwidth: { type: "string", alias: "b", description: "target bandwidth, e.g. 5M (UDP pacing, bits/s)" },
  len: { type: "string", alias: "l", description: "read/write block size, e.g. 128K (bytes; K/M/G are 1024-based)" },
  interval: { type: "string", alias: "i", default: "1", description: "seconds between periodic reports (0 disables)" },
  json: { type: "boolean", default: false, description: "output results as JSON" },
} as const;

const clientCmd = defineCommand({
  meta: { name: "client", description: "Run a speed test against a netsu/iperf3 server" },
  args: clientArgsDef,
  async run({ args, rawArgs }) {
    try {
      const unknown = findUnknownFlag(rawArgs, clientArgsDef);
      if (unknown) throw new Error(`unknown option: ${unknown}`);
      if (args.udp && args.ws) throw new Error("--udp and --ws are mutually exclusive");

      const port = parsePort(args.port, rawArgs);
      const duration = parseIntArg(args.time, "time", 1, negativeHintFor("time", rawArgs, ["-t", "--time"]));
      const parallel = parseIntArg(
        args.parallel,
        "parallel",
        1,
        negativeHintFor("parallel", rawArgs, ["-P", "--parallel"]),
      );
      const intervalSec = parseIntArg(
        args.interval,
        "interval",
        0,
        negativeHintFor("interval", rawArgs, ["-i", "--interval"]),
      );
      const len = args.len !== undefined ? parseByteSize(requireSingleValue(args.len, "len")) : undefined;
      const bandwidth =
        args.bandwidth !== undefined ? parseBandwidth(requireSingleValue(args.bandwidth, "bandwidth")) : undefined;

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
  {
    // citty's default showUsage always writes to stdout (e.g. on a missing
    // required arg, such as `client --json` with no host) — that breaks
    // --json's "nothing but JSON on stdout" contract even though no code in
    // this file ever ran. Route it to stderr whenever --json is present on
    // the command line, mirroring how every other error path here behaves.
    showUsage: async (cmd, parent) => {
      const text = await renderUsage(cmd, parent);
      const jsonMode = process.argv.some((a) => a === "--json" || a.startsWith("--json="));
      if (jsonMode) {
        console.error(text);
      } else {
        console.log(text);
      }
    },
  },
);
