import { execSync, spawn } from "node:child_process";
import { createServer } from "node:net";

export const HAS_IPERF3 = (() => {
  try {
    execSync("iperf3 --version", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

/** Ask the OS for a currently free loopback TCP port. */
export function nextPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const reservation = createServer();
    reservation.once("error", reject);
    reservation.listen(0, "127.0.0.1", () => {
      const address = reservation.address();
      if (!address || typeof address === "string") {
        reservation.close();
        reject(new Error("ephemeral test listener has no numeric address"));
        return;
      }
      const { port } = address;
      reservation.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
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
 * If the child exits early because the ephemeral port was claimed between our
 * reservation closing and iperf3 binding, retry on a newly allocated port and
 * return that actual port to the caller.
 */
export async function spawnIperf3Server(
  extra: string[] = [],
  retriesLeft = 2,
): Promise<{ port: number; kill: () => void }> {
  const port = await nextPort();
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
        resolve(spawnIperf3Server(extra, retriesLeft - 1));
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
        resolve({ port, kill: () => proc.kill("SIGKILL") });
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
