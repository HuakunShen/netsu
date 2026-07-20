import { execSync, spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

// This suite drives the *built* CLI (dist/cli.mjs), not src/cli.ts via `bun
// run` — --json stdout purity, exit codes, and SIGINT handling are meant to
// hold for what actually ships. Don't assume a prior manual `bun run build`
// left dist/ fresh: rebuild explicitly so this file is correct standalone
// (e.g. `bun run test tests/cli.test.ts` on its own).
const PKG_ROOT = fileURLToPath(new URL("..", import.meta.url));
const CLI = fileURLToPath(new URL("../dist/cli.mjs", import.meta.url));

beforeAll(() => {
  // Explicit stdio (rather than the execSync default) so a successful build
  // stays silent instead of bleeding tsdown's banner/warnings into the test
  // run's output — execSync auto-forwards stderr to the parent unless stdio
  // is given explicitly. On failure the captured buffers still surface via
  // the thrown error's `.stdout`/`.stderr`.
  execSync("bun run build", { cwd: PKG_ROOT, stdio: ["ignore", "pipe", "pipe"] });
}, 30_000);

/** Ask the OS for a free port, then release it immediately. Avoids the
 * shared 5210-5260 range in tests/helpers.ts, which partitions by vitest
 * worker index and has proven fragile (~50% EADDRINUSE under concurrency in
 * Task 10) — an OS-assigned ephemeral port doesn't have that collision
 * class. There's a small bind-then-close-then-rebind race in principle, but
 * it's the standard technique and vastly more robust than a fixed window. */
function getEphemeralPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      const port = typeof address === "object" && address ? address.port : 0;
      probe.close(() => resolve(port));
    });
  });
}

interface Proc {
  child: ChildProcess;
  stdout: () => string;
  stderr: () => string;
}

const procs: Proc[] = [];

afterEach(async () => {
  // Await each kill rather than firing SIGKILL and moving on: an unawaited
  // kill can let a slow-to-die process overlap the next test (e.g. still
  // holding the port it was told to release), which is exactly the kind of
  // cross-test interference this suite is trying to eliminate.
  const toKill = procs.splice(0, procs.length);
  await Promise.all(
    toKill.map(({ child }) => {
      if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
      return new Promise<void>((resolve) => {
        child.once("exit", () => resolve());
        child.kill("SIGKILL");
      });
    }),
  );
});

/** Spawn the built CLI. Both stdout and stderr are drained into buffers as
 * data arrives (not just on close), so a chatty child can never block on a
 * full pipe waiting for a reader that only shows up at the end. */
function run(args: string[]): Proc {
  const child = spawn(process.execPath, [CLI, ...args], { stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout!.on("data", (d: Buffer) => (stdout += d.toString()));
  child.stderr!.on("data", (d: Buffer) => (stderr += d.toString()));
  const proc: Proc = { child, stdout: () => stdout, stderr: () => stderr };
  procs.push(proc);
  return proc;
}

/** Resolve once `needle` shows up in `proc`'s stdout, or reject on an early
 * exit / timeout — whichever comes first. Always detaches its own listeners
 * before settling, on every path, so it never leaves a stray `data`/`exit`
 * listener attached to the child after this call is done with it. */
function waitForOutput(proc: Proc, needle: string, timeoutMs = 8000): Promise<void> {
  return new Promise((resolve, reject) => {
    const onData = (d: Buffer) => {
      if (d.toString().includes(needle)) {
        cleanup();
        resolve();
      }
    };
    const onExit = (code: number | null) => {
      cleanup();
      reject(new Error(`process exited (code ${code}) before "${needle}" appeared in stdout`));
    };
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`no "${needle}" in stdout within ${timeoutMs}ms`));
    }, timeoutMs);
    function cleanup() {
      clearTimeout(timer);
      proc.child.stdout!.off("data", onData);
      proc.child.off("exit", onExit);
    }
    proc.child.stdout!.on("data", onData);
    proc.child.once("exit", onExit);
  });
}

function runToCompletion(args: string[]): Promise<{ code: number; stdout: string; stderr: string }> {
  const proc = run(args);
  return new Promise((resolve) => {
    proc.child.on("close", (code) => {
      resolve({ code: code ?? -1, stdout: proc.stdout(), stderr: proc.stderr() });
    });
  });
}

/** Resolve when `proc` exits, or reject after `timeoutMs` — used by the
 * SIGINT test to assert the server actually exits promptly rather than
 * wedging (the CLI-layer version of Task 9's livelock class). */
function waitForExit(proc: Proc, timeoutMs = 5000): Promise<number | null> {
  return new Promise((resolve, reject) => {
    if (proc.child.exitCode !== null || proc.child.signalCode !== null) {
      resolve(proc.child.exitCode);
      return;
    }
    const timer = setTimeout(() => reject(new Error(`process did not exit within ${timeoutMs}ms`)), timeoutMs);
    proc.child.once("exit", (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
}

describe("cli", () => {
  it("server + client run a tcp test end to end", async () => {
    const port = await getEphemeralPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");
    const { code, stdout } = await runToCompletion(["client", "127.0.0.1", "-p", String(port), "-t", "1"]);
    expect(code).toBe(0);
    expect(stdout).toContain("sender");
    expect(stdout).toContain("receiver");
  }, 20000);

  it("--json emits parseable iperf3-shaped output, with intervals, and nothing on stderr", async () => {
    const port = await getEphemeralPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");
    const { code, stdout, stderr } = await runToCompletion([
      "client",
      "127.0.0.1",
      "-p",
      String(port),
      "-t",
      "2",
      "-i",
      "1",
      "--json",
    ]);
    expect(code).toBe(0);
    expect(stderr).toBe("");
    const parsed = JSON.parse(stdout) as {
      intervals: unknown[];
      end: { sum_sent: { bytes: number }; sum_received: { bytes: number } };
    };
    expect(parsed.end.sum_sent.bytes).toBeGreaterThan(0);
    expect(parsed.end.sum_received.bytes).toBeGreaterThan(0);
    expect(parsed.intervals.length).toBeGreaterThan(0);
  }, 20000);

  it("connection refused exits non-zero with empty stdout under --json (error goes to stderr)", async () => {
    // A freshly-released ephemeral port: bind-then-close guarantees nothing
    // is listening on it at the moment we connect.
    const port = await getEphemeralPort();
    const { code, stdout, stderr } = await runToCompletion([
      "client",
      "127.0.0.1",
      "-p",
      String(port),
      "-t",
      "1",
      "--json",
    ]);
    expect(code).not.toBe(0);
    expect(stdout).toBe("");
    expect(stderr.toLowerCase()).toContain("econnrefused");
  }, 10000);

  describe("argument validation rejects bad flags before any network I/O", () => {
    it("-P 0 (zero parallel streams)", async () => {
      const { code, stdout, stderr } = await runToCompletion([
        "client",
        "127.0.0.1",
        "-p",
        "5201",
        "-t",
        "1",
        "-P",
        "0",
      ]);
      expect(code).not.toBe(0);
      expect(stdout).toBe("");
      expect(stderr).toContain("invalid parallel");
    }, 10000);

    it("-t 0 (zero duration)", async () => {
      const { code, stdout, stderr } = await runToCompletion(["client", "127.0.0.1", "-p", "5201", "-t", "0"]);
      expect(code).not.toBe(0);
      expect(stdout).toBe("");
      expect(stderr).toContain("invalid time");
    }, 10000);

    it("-b fast (unparseable bandwidth)", async () => {
      const { code, stdout, stderr } = await runToCompletion([
        "client",
        "127.0.0.1",
        "-p",
        "5201",
        "-t",
        "1",
        "-b",
        "fast",
      ]);
      expect(code).not.toBe(0);
      expect(stdout).toBe("");
      expect(stderr).toContain("invalid bandwidth");
    }, 10000);
  });

  it("SIGINT during an active test exits the server and immediately frees the port (no wedge)", async () => {
    const port = await getEphemeralPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");

    // Long-running client so the server is mid-test (an active session,
    // sockets attached) at the moment we signal, not idle between runs.
    const client = run(["client", "127.0.0.1", "-p", String(port), "-t", "10"]);
    // Default interval reporting (-i 1) prints a "[SUM]" line once the first
    // second of transfer has actually happened — that's our signal that the
    // server has a live, active session rather than just an accepted socket.
    await waitForOutput(client, "[SUM]");

    server.child.kill("SIGINT");
    const exitCode = await waitForExit(server, 5000);
    expect(exitCode).toBe(0);

    // The regression this guards against (Task 9's livelock class
    // reappearing at the CLI layer) is a server whose close() never
    // releases the OS-level socket, so a rebind on the same port either
    // hangs or fails with EADDRINUSE. Rebind immediately, with a short
    // timeout, to prove the port was actually released.
    const rebound = run(["server", "-p", String(port)]);
    await waitForOutput(rebound, "listening", 3000);
  }, 20000);
});
