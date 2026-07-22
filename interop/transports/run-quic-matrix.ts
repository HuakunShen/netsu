#!/usr/bin/env bun
/** Isolated rs<->rs native QUIC correctness matrix. */
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

const COMPOSE_FILE = path.join(import.meta.dir, "docker-compose.quic.yml");
const COMPOSE = ["compose", "-f", COMPOSE_FILE];
const RESULTS_DIR = path.join(import.meta.dir, "results");
const ABSURD_BPS = 1e12;
const BYTE_DRIFT_LIMIT = 0.02;

export type QuicProfile = "baseline" | "constrained" | "lossy";
export type QuicDirection = "upload" | "reverse";

export interface QuicCell {
  name: string;
  profile: QuicProfile;
  direction: QuicDirection;
  parallel: 1 | 4;
  durationSeconds: number;
  timeoutMs: number;
}

function cell(
  profile: QuicProfile,
  direction: QuicDirection,
  parallel: 1 | 4,
): QuicCell {
  const durationSeconds = 3;
  return {
    name: `${profile}-${direction}-p${parallel}`,
    profile,
    direction,
    parallel,
    durationSeconds,
    timeoutMs: (durationSeconds + 25) * 1_000,
  };
}

export function buildQuicMatrix(): QuicCell[] {
  return [
    cell("baseline", "upload", 1),
    cell("baseline", "reverse", 1),
    cell("baseline", "upload", 4),
    cell("baseline", "reverse", 4),
    cell("constrained", "upload", 1),
    cell("lossy", "upload", 1),
  ];
}

interface CommandResult {
  code: number;
  stdout: string;
  stderr: string;
}

function command(
  executable: string,
  args: string[],
  timeoutMs: number,
): Promise<CommandResult> {
  return new Promise((resolve) => {
    const child = spawn(executable, args, {
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    child.stdout.on("data", (chunk) => (stdout += chunk.toString()));
    child.stderr.on("data", (chunk) => (stderr += chunk.toString()));
    child.on("error", (error) => (stderr += `\n${error.message}`));
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.on("close", (code) => {
      clearTimeout(timer);
      if (timedOut) stderr += `\n[runner] timed out after ${timeoutMs}ms`;
      resolve({ code: code ?? -1, stdout, stderr });
    });
  });
}

function docker(args: string[], timeoutMs: number): Promise<CommandResult> {
  return command("docker", [...COMPOSE, ...args], timeoutMs);
}

function waitForReadiness(
  child: ReturnType<typeof spawn>,
  getStdout: () => string,
  getStderr: () => string,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(
        new Error(`server readiness timed out: ${getStderr().slice(0, 500)}`),
      );
    }, 10_000);
    const onData = () => {
      if (getStdout().includes("netsu server listening")) {
        cleanup();
        resolve();
      }
    };
    const onClose = (code: number | null) => {
      cleanup();
      reject(
        new Error(
          `server exited before readiness (${code}): ${getStderr().slice(0, 500)}`,
        ),
      );
    };
    const cleanup = () => {
      clearTimeout(timeout);
      child.stdout?.off("data", onData);
      child.off("close", onClose);
    };
    child.stdout?.on("data", onData);
    child.on("close", onClose);
    onData();
  });
}

interface NetsuResult {
  end?: {
    sum_sent?: { bytes?: number; bits_per_second?: number };
    sum_received?: { bytes?: number; bits_per_second?: number };
  };
  connection?: {
    transport?: string;
    path?: string;
    handshake_ms?: number;
    streams?: number;
  };
}

function assertResult(cell: QuicCell, raw: string): NetsuResult {
  let result: NetsuResult;
  try {
    result = JSON.parse(raw) as NetsuResult;
  } catch {
    throw new Error(`stdout is not JSON: ${raw.slice(0, 400)}`);
  }

  const sent = result.end?.sum_sent?.bytes ?? 0;
  const received = result.end?.sum_received?.bytes ?? 0;
  const bps = Math.max(
    result.end?.sum_sent?.bits_per_second ?? 0,
    result.end?.sum_received?.bits_per_second ?? 0,
  );
  if (sent <= 0 || received <= 0) {
    throw new Error(`zero transfer: sent=${sent} received=${received}`);
  }
  if (!Number.isFinite(bps) || bps <= 0 || bps >= ABSURD_BPS) {
    throw new Error(`implausible throughput: ${bps} bits/s`);
  }
  const drift = Math.abs(sent - received) / Math.max(sent, received);
  if (drift > BYTE_DRIFT_LIMIT) {
    throw new Error(
      `byte drift ${(drift * 100).toFixed(2)}% exceeds 2%: sent=${sent} received=${received}`,
    );
  }
  if (
    result.connection?.transport !== "quic" ||
    result.connection.path !== "direct"
  ) {
    throw new Error(
      `wrong connection classification: ${JSON.stringify(result.connection)}`,
    );
  }
  if (result.connection.streams !== cell.parallel) {
    throw new Error(
      `stream count ${result.connection.streams} != ${cell.parallel}`,
    );
  }
  const handshakeMs = result.connection.handshake_ms;
  if (
    !Number.isFinite(handshakeMs) ||
    (handshakeMs ?? -1) < 0 ||
    (handshakeMs ?? 10_001) > 10_000
  ) {
    throw new Error(`invalid or unbounded handshake: ${handshakeMs}ms`);
  }
  return result;
}

async function writeFailure(
  cell: QuicCell,
  reason: unknown,
  client: CommandResult | undefined,
  serverStdout: string,
  serverStderr: string,
): Promise<void> {
  await mkdir(RESULTS_DIR, { recursive: true });
  await writeFile(
    path.join(RESULTS_DIR, `${cell.name}.json`),
    JSON.stringify(
      {
        cell,
        error: reason instanceof Error ? reason.message : String(reason),
        client,
        server: { stdout: serverStdout, stderr: serverStderr },
      },
      null,
      2,
    ),
  );
}

let portCounter = 6401;

async function runCell(cell: QuicCell): Promise<void> {
  const port = portCounter++;
  const server = spawn(
    "docker",
    [
      ...COMPOSE,
      "exec",
      "-T",
      "quic-server",
      "/usr/local/bin/netsu",
      "server",
      "-p",
      String(port),
      "--quic",
      "--quic-self-signed",
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let serverStdout = "";
  let serverStderr = "";
  let clientResult: CommandResult | undefined;
  server.stdout.on("data", (chunk) => (serverStdout += chunk.toString()));
  server.stderr.on("data", (chunk) => (serverStderr += chunk.toString()));

  try {
    await waitForReadiness(
      server,
      () => serverStdout,
      () => serverStderr,
    );
    const clientArgs = [
      "exec",
      "-T",
      "quic-client",
      "/usr/local/bin/netem-entrypoint",
      cell.profile,
      "--",
      "/usr/local/bin/netsu",
      "client",
      "quic-server",
      "-p",
      String(port),
      "-t",
      String(cell.durationSeconds),
      "-P",
      String(cell.parallel),
      "--quic",
      "--quic-insecure",
      "--json",
    ];
    if (cell.direction === "reverse") clientArgs.push("-R");
    clientResult = await docker(clientArgs, cell.timeoutMs);
    if (clientResult.code !== 0) {
      throw new Error(
        `client exit ${clientResult.code}: ${clientResult.stderr.slice(0, 500)}`,
      );
    }
    const result = assertResult(cell, clientResult.stdout);
    const mbps =
      Math.max(
        result.end?.sum_sent?.bits_per_second ?? 0,
        result.end?.sum_received?.bits_per_second ?? 0,
      ) / 1e6;
    console.log(`PASS  ${cell.name}  ${mbps.toFixed(1)} Mbit/s`);
  } catch (error) {
    await writeFailure(cell, error, clientResult, serverStdout, serverStderr);
    throw error;
  } finally {
    await docker(
      [
        "exec",
        "-T",
        "quic-server",
        "pkill",
        "-TERM",
        "-f",
        `netsu server -p ${port}`,
      ],
      5_000,
    ).catch(() => undefined);
    server.kill("SIGTERM");
  }
}

async function main(): Promise<void> {
  const requested = process.env.QUIC_CASE;
  const matrix = buildQuicMatrix();
  const cells = requested
    ? matrix.filter((cell) => cell.name === requested)
    : matrix;
  if (requested && cells.length === 0) {
    throw new Error(
      `unknown QUIC_CASE=${requested}; choose one of: ${matrix.map((cell) => cell.name).join(", ")}`,
    );
  }
  console.log(`running ${cells.length} QUIC correctness cell(s)`);
  for (const current of cells) await runCell(current);
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  });
}
