import type { IntervalReport } from "./stats.ts";

const SUFFIXED_NUMBER = /^(\d+(?:\.\d+)?)([kKmMgG])?$/;

/** "5M" → 5_000_000 bits/s. K/M/G are decimal, like iperf3's -b. */
export function parseBandwidth(s: string): number {
  const m = SUFFIXED_NUMBER.exec(s);
  if (!m) throw new Error(`invalid bandwidth: ${s}`);
  const mult = { k: 1e3, m: 1e6, g: 1e9 }[(m[2] ?? "").toLowerCase()] ?? 1;
  return Math.round(Number(m[1]) * mult);
}

/**
 * "128K" → 131072 bytes. Unlike `-b`'s decimal K/M/G, iperf3's `-l` block
 * size suffixes are 1024-based — keep the two separate rather than unifying
 * them (confirmed empirically: `iperf3 -b 1M` reports exactly 1.00 Mbits/sec,
 * i.e. decimal, while byte-size flags like `-l`/`-w` follow the traditional
 * KiB/MiB/GiB convention).
 */
export function parseByteSize(s: string): number {
  const m = SUFFIXED_NUMBER.exec(s);
  if (!m) throw new Error(`invalid len: ${s}`);
  const mult = { k: 1024, m: 1024 ** 2, g: 1024 ** 3 }[(m[2] ?? "").toLowerCase()] ?? 1;
  const bytes = Math.round(Number(m[1]) * mult);
  if (bytes < 1) throw new Error(`invalid len: ${s}`);
  return bytes;
}

export function formatBytes(n: number): string {
  let value = n;
  const units = ["Bytes", "KBytes", "MBytes", "GBytes", "TBytes"];
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i++;
  }
  const text = value >= 100 || Number.isInteger(value) ? String(Math.round(value)) : value.toFixed(2);
  return `${text} ${units[i]}`;
}

export function formatBits(n: number): string {
  let value = n;
  const units = ["bits/sec", "Kbits/sec", "Mbits/sec", "Gbits/sec"];
  let i = 0;
  while (value >= 1000 && i < units.length - 1) {
    value /= 1000;
    i++;
  }
  const text = value >= 100 || Number.isInteger(value) ? String(Math.round(value)) : value.toFixed(2);
  return `${text} ${units[i]}`;
}

export function intervalLine(r: IntervalReport): string {
  const range = `${r.start.toFixed(2)}-${r.end.toFixed(2)}`.padStart(11);
  return `[SUM] ${range} sec  ${formatBytes(r.bytes).padStart(12)}  ${formatBits(r.bitsPerSecond).padStart(14)}`;
}
