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
// Seeded from process.pid so parallel vitest worker threads (each with a
// separate module registry, hence a separate portCounter) don't all restart
// at 5210 and collide across test files. Wraps within the mandated
// 5210-5260 range; never emits 5201, see global constraints.
let portCounter = PORT_MIN + (process.pid % PORT_RANGE);
/** Unique-ish port per test — never 5201, see global constraints. */
export function nextPort(): number {
  const port = portCounter;
  portCounter = PORT_MIN + ((portCounter - PORT_MIN + 1) % PORT_RANGE);
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
 */
export function spawnIperf3Server(port: number, extra: string[] = []): Promise<() => void> {
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
      if (!settled) {
        settled = true;
        clearTimeout(deadline);
        reject(new Error(`iperf3 -s exited early (code ${code}): ${stderr.slice(0, 300)}`));
      }
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
