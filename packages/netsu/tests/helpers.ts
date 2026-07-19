import { execSync, spawn } from "node:child_process";

export const HAS_IPERF3 = (() => {
  try {
    execSync("iperf3 --version", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

let portCounter = 5210;
/** Unique port per test — never 5201, see global constraints. */
export function nextPort(): number {
  return portCounter++;
}

/** True if something is listening on `port` — checked without connecting. */
function isListening(port: number): boolean {
  try {
    const out = execSync(`lsof -iTCP:${port} -sTCP:LISTEN -n -P`, {
      stdio: ["ignore", "pipe", "ignore"],
    });
    return out.length > 0;
  } catch {
    return false;
  }
}

/**
 * Spawn `iperf3 -s -1` (one-off server); resolves once it is listening.
 *
 * Readiness is detected by polling `lsof` for the LISTEN socket rather than
 * (a) matching the "Server listening" banner on stdout, or (b) probing with
 * a real connect. (a) is unreliable: iperf3's stdout is fully block-buffered
 * when it is not a tty (true for a spawned pipe), so the banner can sit
 * unflushed for the server's whole lifetime. (b) is actively harmful under
 * `-1`: iperf3 treats the very first accepted TCP connection as the control
 * connection for its one-shot test, so a probe that connects and disconnects
 * without sending a cookie makes the server log "unable to receive cookie"
 * and exit immediately — confirmed by observation on this machine (iperf
 * 3.21 / macOS): a subsequent real connect then gets ECONNREFUSED. Polling
 * `lsof` makes no connection at all, so it cannot consume that one-shot slot.
 */
export function spawnIperf3Server(port: number, extra: string[] = []): Promise<() => void> {
  const proc = spawn("iperf3", ["-s", "-1", "-p", String(port), ...extra], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stderr = "";
  proc.stderr.on("data", (d: Buffer) => (stderr += d.toString()));

  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 5000;
    let settled = false;

    proc.on("error", (err) => {
      if (!settled) {
        settled = true;
        reject(err);
      }
    });
    proc.on("exit", (code) => {
      if (!settled) {
        settled = true;
        reject(new Error(`iperf3 -s exited early (code ${code}): ${stderr.slice(0, 300)}`));
      }
    });

    const poll = () => {
      if (settled) return;
      if (isListening(port)) {
        settled = true;
        resolve(() => proc.kill("SIGKILL"));
        return;
      }
      if (Date.now() > deadline) {
        settled = true;
        reject(new Error("iperf3 -s did not start"));
        return;
      }
      setTimeout(poll, 20);
    };
    poll();
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
