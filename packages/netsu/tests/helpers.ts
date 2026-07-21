import { execSync, spawn } from "node:child_process";

export const HAS_IPERF3 = (() => {
  try {
    execSync("iperf3 --version", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

const PORT_MIN = 5210;
const PORT_MAX = 5260;
const PORT_RANGE = PORT_MAX - PORT_MIN + 1;
// Partition the range by vitest's worker index (VITEST_POOL_ID: 1-based,
// dense) rather than process.pid: vitest's default "forks" pool forks
// workers back-to-back, so consecutive workers get consecutive pids, which
// previously mapped to *adjacent* seeds — two port-using test files running
// concurrently would overlap their ~4-port windows almost every time,
// producing a bind conflict ("iperf3 -s exited early") that looked like a
// real regression. VITEST_POOL_ID is dense (1, 2, 3, ...), so multiplying by
// a stride bigger than any one file's port usage keeps windows from
// different workers from overlapping. Wraps within the mandated 5210-5260
// range; never emits 5201, see global constraints.
// Each worker gets its OWN disjoint STRIDE-sized window and cycles within it.
// The previous scheme only offset each worker's *start* but wrapped over the
// full range, so a worker making more than STRIDE nextPort() calls walked into
// the next worker's window and both bound the same port (EADDRINUSE). Cycling
// within a fixed per-worker window keeps them disjoint for up to
// PORT_RANGE/STRIDE concurrent workers (>= any CI runner's core count).
const WORKER_STRIDE = 8;
const WORKER_WINDOWS = Math.floor(PORT_RANGE / WORKER_STRIDE);
const workerBase =
  PORT_MIN + ((Number(process.env.VITEST_POOL_ID ?? 1) - 1) % WORKER_WINDOWS) * WORKER_STRIDE;
let windowOffset = 0;
/** Unique port per test within this worker's window — never 5201, see global constraints. */
export function nextPort(): number {
  const port = workerBase + (windowOffset % WORKER_STRIDE);
  windowOffset++;
  return port;
}

/**
 * Spawn `iperf3 -s -1` (one-off server); resolves once it is listening.
 *
 * Readiness is detected by matching the "Server listening" banner on stdout.
 * iperf3's stdout is fully block-buffered when it is not a tty (true for a
 * spawned pipe), so without help the banner can sit unflushed for the
 * server's whole lifetime — `--forceflush` makes iperf3 flush stdout after
 * every line, so the banner reliably arrives through the pipe in ~2ms.
 * (A real-connect probe was considered and rejected: under `-1`, iperf3
 * treats the very first accepted TCP connection as the control connection
 * for its one-shot test, so a probe that connects and disconnects without
 * sending a cookie makes the server log "unable to receive cookie" and exit
 * immediately — confirmed by observation on this machine (iperf 3.21 /
 * macOS): a subsequent real connect then gets ECONNREFUSED.)
 *
 * Rejects early on the child's `exit` event (with captured stderr) rather
 * than only timing out, so a server that fails to start produces a real
 * diagnostic instead of a silent 5s "did not start".
 *
 * If the child exits early because the port was already bound (belt-and-
 * suspenders on top of the per-worker port partitioning in `nextPort()`,
 * which already keeps concurrent workers' windows from overlapping in the
 * common case), retry once on the next port rather than failing the test.
 */
export function spawnIperf3Server(
  port: number,
  extra: string[] = [],
  retriesLeft = 2,
): Promise<() => void> {
  const proc = spawn(
    "iperf3",
    ["-s", "-1", "-p", String(port), "--forceflush", ...extra],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let stderr = "";
  let stdout = "";
  proc.stderr.on("data", (d: Buffer) => (stderr += d.toString()));

  return new Promise((resolve, reject) => {
    const deadline = setTimeout(() => {
      settled = true;
      proc.kill("SIGKILL");
      reject(new Error("iperf3 -s did not start"));
    }, 5000);
    let settled = false;

    proc.on("error", (err) => {
      if (!settled) {
        settled = true;
        clearTimeout(deadline);
        reject(err);
      }
    });
    proc.on("exit", (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      if (retriesLeft > 0 && /address already in use/i.test(stderr)) {
        resolve(spawnIperf3Server(nextPort(), extra, retriesLeft - 1));
        return;
      }
      reject(new Error(`iperf3 -s exited early (code ${code}): ${stderr.slice(0, 300)}`));
    });
    proc.stdout.on("data", (d: Buffer) => {
      if (settled) return;
      stdout += d.toString();
      if (stdout.includes("Server listening")) {
        settled = true;
        clearTimeout(deadline);
        resolve(() => proc.kill("SIGKILL"));
      }
    });
  });
}

/** Run `iperf3 -c ... --json`, return exit code and parsed output. */
export function runIperf3Client(args: string[]): Promise<{ code: number; json: unknown }> {
  const proc = spawn("iperf3", [...args, "--json"], { stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  proc.stdout.on("data", (d: Buffer) => (out += d.toString()));
  return new Promise((resolve, reject) => {
    proc.on("error", reject);
    proc.on("close", (code) => {
      try {
        resolve({ code: code ?? -1, json: JSON.parse(out) });
      } catch {
        reject(new Error(`iperf3 output not json (exit ${code}): ${out.slice(0, 300)}`));
      }
    });
  });
}
