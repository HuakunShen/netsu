import { spawn, type ChildProcess } from "node:child_process";
import { afterEach, describe, expect, it } from "vitest";
import { nextPort } from "./helpers.ts";

const procs: ChildProcess[] = [];
afterEach(() => {
  while (procs.length) procs.pop()!.kill("SIGKILL");
});

function run(args: string[]): ChildProcess {
  const proc = spawn("bun", ["src/cli.ts", ...args], { stdio: ["ignore", "pipe", "pipe"] });
  procs.push(proc);
  return proc;
}

function waitForOutput(proc: ChildProcess, needle: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`no "${needle}" in output`)), 8000);
    proc.stdout!.on("data", (d: Buffer) => {
      if (d.toString().includes(needle)) {
        clearTimeout(timer);
        resolve();
      }
    });
  });
}

function collect(proc: ChildProcess): Promise<{ code: number; out: string }> {
  let out = "";
  proc.stdout!.on("data", (d: Buffer) => (out += d.toString()));
  return new Promise((resolve) => proc.on("close", (code) => resolve({ code: code ?? -1, out })));
}

describe("cli", () => {
  it("server + client run a tcp test end to end", async () => {
    const port = nextPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");
    const { code, out } = await collect(run(["client", "127.0.0.1", "-p", String(port), "-t", "1"]));
    expect(code).toBe(0);
    expect(out).toContain("sender");
    expect(out).toContain("receiver");
  }, 20000);

  it("--json emits parseable iperf3-shaped output", async () => {
    const port = nextPort();
    const server = run(["server", "-p", String(port)]);
    await waitForOutput(server, "listening");
    const { code, out } = await collect(
      run(["client", "127.0.0.1", "-p", String(port), "-t", "1", "--json"]),
    );
    expect(code).toBe(0);
    const parsed = JSON.parse(out) as {
      end: { sum_sent: { bytes: number }; sum_received: { bytes: number } };
    };
    expect(parsed.end.sum_sent.bytes).toBeGreaterThan(0);
    expect(parsed.end.sum_received.bytes).toBeGreaterThan(0);
  }, 20000);
});
