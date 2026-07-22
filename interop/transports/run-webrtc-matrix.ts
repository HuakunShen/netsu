#!/usr/bin/env bun
/** Self-contained Worker + Rust + Chromium direct-only WebRTC correctness matrix. */
import { spawn } from "node:child_process";
import { mkdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const COMPOSE_FILE = path.join(SCRIPT_DIR, "docker-compose.yml");
const COMPOSE_PROJECT =
  process.env.COMPOSE_PROJECT_NAME ?? "netsu-webrtc-e2e-manual";
const COMPOSE = ["compose", "-p", COMPOSE_PROJECT, "-f", COMPOSE_FILE];
const SIGNAL_URL = "http://signal:8787/v1/signal";
const RESULTS_DIR = path.join(SCRIPT_DIR, "results");
const DURATION_SECONDS = 1;
const SUCCESS_TIMEOUT_MS = 40_000;
const BLOCKED_TIMEOUT_MS = 26_000;
const CONTAINER_RESET_TIMEOUT_MS = 60_000;
const CELL_PROCESS_TIMEOUT_MS = 120_000;
const ABSURD_BPS = 1e12;
const DIRECT_WARNING =
  "warning: WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run";

export type WebRtcPeer = "rust" | "chromium";
export type WebRtcDirection = "upload" | "reverse";

export interface WebRtcCell {
  name: string;
  peer: WebRtcPeer;
  direction: WebRtcDirection;
  parallel: 1 | 4;
  blocked: boolean;
}

function cell(
  name: string,
  peer: WebRtcPeer,
  direction: WebRtcDirection,
  parallel: 1 | 4,
  blocked = false,
): WebRtcCell {
  return { name, peer, direction, parallel, blocked };
}

export function buildWebRtcMatrix(): WebRtcCell[] {
  return [
    cell("rust-upload-p1", "rust", "upload", 1),
    cell("rust-upload-p4", "rust", "upload", 4),
    cell("rust-reverse-p1", "rust", "reverse", 1),
    cell("rust-reverse-p4", "rust", "reverse", 4),
    cell("chromium-upload-p1", "chromium", "upload", 1),
    cell("chromium-upload-p4", "chromium", "upload", 4),
    cell("chromium-reverse-p1", "chromium", "reverse", 1),
    cell("rust-blocked-upload-p1", "rust", "upload", 1, true),
    cell("chromium-blocked-upload-p1", "chromium", "upload", 1, true),
  ];
}

export function buildIsolatedCaseEnvironments(
  base: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv[] {
  return buildWebRtcMatrix().map((entry) => ({
    ...base,
    WEBRTC_CASE: entry.name,
  }));
}

export function peerServicesForCell(
  cell: WebRtcCell,
): ["rs-server", "rs-client" | "browser"] {
  return ["rs-server", cell.peer === "rust" ? "rs-client" : "browser"];
}

export function redactArtifact(input: string): string {
  return input
    .replace(
      /listener[_-]?secret\s*=\s*(?:"[^"]*"|'[^']*'|\S+)/gi,
      "redacted=[REDACTED_SIGNALING_MATERIAL]",
    )
    .replace(
      /"(?:listener[_-]?secret|secret|sdp)"\s*:\s*"(?:\\.|[^"\\])*"/gi,
      '"redacted":"[REDACTED_SIGNALING_MATERIAL]"',
    )
    .replace(/candidate:[^"\\\r\n]*/gi, "[REDACTED_CANDIDATE]")
    .replace(/\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?\b/g, "[REDACTED_ADDRESS]")
    .replace(/\b(?:[a-f0-9]{1,4}:){2,}[a-f0-9:%.-]+\b/gi, "[REDACTED_ADDRESS]")
    .replace(/\b(?:local|remote)_addr\s*[=:]\s*\S+/gi, "[REDACTED_ADDRESS]");
}

interface CommandResult {
  code: number;
  stdout: string;
  stderr: string;
  timedOut: boolean;
}

function command(
  executable: string,
  args: string[],
  timeoutMs: number,
  environment: NodeJS.ProcessEnv = process.env,
): Promise<CommandResult> {
  return new Promise((resolve) => {
    const child = spawn(executable, args, {
      stdio: ["ignore", "pipe", "pipe"],
      env: environment,
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
      resolve({ code: code ?? -1, stdout, stderr, timedOut });
    });
  });
}

export function observeChildClose(
  child: ReturnType<typeof spawn>,
): Promise<void> {
  return new Promise((resolve) => child.once("close", () => resolve()));
}

async function waitForObservedClose(
  closed: Promise<void>,
  timeoutMs: number,
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      closed,
      new Promise<void>((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(`child process did not close within ${timeoutMs}ms`),
            ),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function docker(args: string[], timeoutMs = 15_000): Promise<CommandResult> {
  return command("docker", [...COMPOSE, ...args], timeoutMs);
}

function parseJson(raw: string): Record<string, unknown> {
  try {
    return JSON.parse(raw.trim()) as Record<string, unknown>;
  } catch {
    throw new Error(`stdout is not one JSON object: ${raw.slice(0, 400)}`);
  }
}

function numberAt(
  object: Record<string, unknown>,
  pathParts: string[],
): number {
  let value: unknown = object;
  for (const part of pathParts) {
    if (typeof value !== "object" || value === null) return Number.NaN;
    value = (value as Record<string, unknown>)[part];
  }
  return typeof value === "number" ? value : Number.NaN;
}

function objectAt(
  object: Record<string, unknown>,
  key: string,
): Record<string, unknown> {
  const value = object[key];
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`missing object field ${key}`);
  }
  return value as Record<string, unknown>;
}

function assertDirectConnection(
  connection: Record<string, unknown>,
  parallel: number,
): void {
  if (connection.transport !== "webrtc" || connection.path !== "direct") {
    throw new Error(
      `wrong connection classification: ${JSON.stringify(connection)}`,
    );
  }
  for (const field of ["local_candidate_type", "remote_candidate_type"]) {
    if (!["host", "srflx", "prflx"].includes(String(connection[field]))) {
      throw new Error(`non-direct ${field}: ${String(connection[field])}`);
    }
  }
  if (
    !connection.ice_protocol ||
    String(connection.ice_protocol) === "unknown"
  ) {
    throw new Error("selected ICE protocol is missing or unknown");
  }
  if (connection.addresses_included !== false) {
    throw new Error("container E2E unexpectedly included candidate addresses");
  }
  const streams = Number(connection.streams);
  if (streams !== parallel) {
    throw new Error(`stream count ${streams} != ${parallel}`);
  }
}

function assertSuccessfulResult(
  cell: WebRtcCell,
  result: Record<string, unknown>,
): number {
  const browser = cell.peer === "chromium";
  const sent = browser
    ? numberAt(result, ["sent_bytes"])
    : numberAt(result, ["end", "sum_sent", "bytes"]);
  const received = browser
    ? numberAt(result, ["received_bytes"])
    : numberAt(result, ["end", "sum_received", "bytes"]);
  const sendBps = browser
    ? numberAt(result, ["send_bits_per_second"])
    : numberAt(result, ["end", "sum_sent", "bits_per_second"]);
  const receiveBps = browser
    ? numberAt(result, ["receive_bits_per_second"])
    : numberAt(result, ["end", "sum_received", "bits_per_second"]);
  if (!(sent > 0) || !(received > 0)) {
    throw new Error(`zero transfer: sent=${sent} received=${received}`);
  }
  const bps = Math.max(sendBps, receiveBps);
  if (!Number.isFinite(bps) || bps <= 0 || bps >= ABSURD_BPS) {
    throw new Error(`invalid throughput: ${bps}`);
  }
  const drift = Math.abs(sent - received) / Math.max(sent, received);
  const limit = browser ? 0.05 : 0.02;
  if (drift > limit) {
    throw new Error(
      `byte drift ${(drift * 100).toFixed(2)}% exceeds ${limit * 100}%`,
    );
  }
  if (browser && Number(result.parallel) !== cell.parallel) {
    throw new Error(
      `browser parallel=${String(result.parallel)} != ${cell.parallel}`,
    );
  }
  assertDirectConnection(objectAt(result, "connection"), cell.parallel);
  return bps;
}

function assertBlockedResult(
  cell: WebRtcCell,
  commandResult: CommandResult,
): void {
  if (commandResult.timedOut)
    throw new Error("blocked path exceeded its bounded deadline");
  if (commandResult.code !== 4) {
    throw new Error(
      `blocked ${cell.peer} exited ${commandResult.code}: ${commandResult.stderr.slice(0, 500)}`,
    );
  }
  if (!commandResult.stderr.includes(DIRECT_WARNING)) {
    throw new Error("blocked path did not emit the direct-only warning");
  }
  if (commandResult.stdout.includes("bits_per_second")) {
    throw new Error("blocked path emitted throughput fields");
  }
  const result = parseJson(commandResult.stdout);
  const error = objectAt(result, "error");
  if (
    error.transport !== "webrtc" ||
    error.kind !== "direct_path_unavailable"
  ) {
    throw new Error(`wrong blocked-path error: ${JSON.stringify(error)}`);
  }
}

interface ServerProcess {
  child: ReturnType<typeof spawn>;
  closed: Promise<void>;
  stdout: string;
  stderr: string;
}

async function startServer(): Promise<{ server: ServerProcess; code: string }> {
  const child = spawn(
    "docker",
    [
      ...COMPOSE,
      "exec",
      "-T",
      "rs-server",
      "/usr/local/bin/netsu",
      "server",
      "--webrtc",
      "--signal-url",
      SIGNAL_URL,
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  const server: ServerProcess = {
    child,
    closed: observeChildClose(child),
    stdout: "",
    stderr: "",
  };
  child.stdout.on("data", (chunk) => (server.stdout += chunk.toString()));
  child.stderr.on("data", (chunk) => (server.stderr += chunk.toString()));

  const code = await new Promise<string>((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(
        new Error(
          `server room readiness timed out: ${server.stderr.slice(0, 500)}`,
        ),
      );
    }, 15_000);
    const onData = () => {
      const match = server.stdout.match(
        /^code:\s+([A-Z0-9]{4}-[A-Z0-9]{4})\s*$/m,
      );
      if (match) {
        cleanup();
        resolve(match[1]);
      }
    };
    const onClose = (exitCode: number | null) => {
      cleanup();
      reject(
        new Error(
          `server exited before room readiness (${exitCode}): ${server.stderr.slice(0, 500)}`,
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
  return { server, code };
}

export function buildUdpBlockRule(): string[] {
  return ["-p", "udp", "-d", "rs-server", "-j", "REJECT"];
}

async function setUdpBlocked(service: string, blocked: boolean): Promise<void> {
  const rule = buildUdpBlockRule();
  if (blocked) {
    const result = await docker([
      "exec",
      "-T",
      service,
      "iptables",
      "-I",
      "OUTPUT",
      "1",
      ...rule,
    ]);
    if (result.code !== 0)
      throw new Error(`could not block UDP: ${result.stderr}`);
    return;
  }
  await docker([
    "exec",
    "-T",
    service,
    "iptables",
    "-D",
    "OUTPUT",
    ...rule,
  ]).catch(() => undefined);
}

function clientArgs(cell: WebRtcCell, code: string): string[] {
  if (cell.peer === "rust") {
    const args = [
      "exec",
      "-T",
      "rs-client",
      "/usr/local/bin/netsu",
      "client",
      code,
      "--webrtc",
      "--signal-url",
      SIGNAL_URL,
      "-t",
      String(DURATION_SECONDS),
      "-P",
      String(cell.parallel),
      "-l",
      "64K",
      "--json",
    ];
    if (cell.direction === "reverse") args.push("-R");
    return args;
  }
  const args = [
    "exec",
    "-T",
    "browser",
    "node",
    "/app/run-browser-peer.mjs",
    "--signal-url",
    SIGNAL_URL,
    "--code",
    code,
    "--duration",
    String(DURATION_SECONDS),
    "--parallel",
    String(cell.parallel),
    "--length",
    "65536",
  ];
  if (cell.direction === "reverse") args.push("--reverse");
  return args;
}

async function writeFailure(
  cell: WebRtcCell,
  reason: unknown,
  commandResult: CommandResult | undefined,
  server: ServerProcess | undefined,
): Promise<void> {
  await mkdir(RESULTS_DIR, { recursive: true });
  const payload = redactArtifact(
    JSON.stringify(
      {
        cell,
        error: reason instanceof Error ? reason.message : String(reason),
        client: commandResult,
        server: server && { stdout: server.stdout, stderr: server.stderr },
      },
      null,
      2,
    ),
  );
  await writeFile(path.join(RESULTS_DIR, `${cell.name}.json`), payload);
  const logs = await docker(["logs", "--no-color"], 10_000);
  await writeFile(
    path.join(RESULTS_DIR, "compose-webrtc.log"),
    redactArtifact(`${logs.stdout}\n${logs.stderr}`),
  );
}

async function stopServer(server: ServerProcess | undefined): Promise<void> {
  await docker([
    "exec",
    "-T",
    "rs-server",
    "pkill",
    "-TERM",
    "-x",
    "netsu",
  ]).catch(() => undefined);
  if (!server) return;
  if (server.child.exitCode === null && server.child.signalCode === null) {
    server.child.kill("SIGTERM");
  }
  try {
    await waitForObservedClose(server.closed, 5_000);
  } catch {
    server.child.kill("SIGKILL");
    await waitForObservedClose(server.closed, 1_000).catch(() => undefined);
  }
}

async function stopClient(service: "rs-client" | "browser"): Promise<void> {
  const args =
    service === "browser"
      ? ["exec", "-T", service, "pkill", "-TERM", "-f", "run-browser-peer.mjs"]
      : ["exec", "-T", service, "pkill", "-TERM", "-x", "netsu"];
  await docker(args).catch(() => undefined);
}

async function resetPeerContainers(cell: WebRtcCell): Promise<void> {
  const result = await docker(
    ["up", "-d", "--no-deps", "--force-recreate", ...peerServicesForCell(cell)],
    CONTAINER_RESET_TIMEOUT_MS,
  );
  if (result.code !== 0) {
    throw new Error(`could not reset peer containers: ${result.stderr}`);
  }
}

async function runCell(cell: WebRtcCell): Promise<void> {
  let server: ServerProcess | undefined;
  let commandResult: CommandResult | undefined;
  const clientService = cell.peer === "rust" ? "rs-client" : "browser";
  try {
    await resetPeerContainers(cell);
    const started = await startServer();
    server = started.server;
    if (cell.blocked) await setUdpBlocked(clientService, true);
    commandResult = await docker(
      clientArgs(cell, started.code),
      cell.blocked ? BLOCKED_TIMEOUT_MS : SUCCESS_TIMEOUT_MS,
    );
    if (cell.blocked) {
      assertBlockedResult(cell, commandResult);
      console.log(`PASS  ${cell.name}  direct path rejected before throughput`);
      return;
    }
    if (commandResult.code !== 0) {
      throw new Error(
        `client exit ${commandResult.code}: ${commandResult.stderr.slice(0, 500)}`,
      );
    }
    const bps = assertSuccessfulResult(cell, parseJson(commandResult.stdout));
    console.log(`PASS  ${cell.name}  ${(bps / 1e6).toFixed(1)} Mbit/s`);
  } catch (error) {
    await writeFailure(cell, error, commandResult, server);
    throw error;
  } finally {
    if (cell.blocked) await setUdpBlocked(clientService, false);
    await stopClient(clientService);
    await stopServer(server);
  }
}

async function assertSignalReady(): Promise<void> {
  const result = await docker([
    "exec",
    "-T",
    "signal",
    "node",
    "-e",
    "fetch('http://127.0.0.1:8787/healthz').then(async r=>{if(!r.ok)throw new Error(await r.text())})",
  ]);
  if (result.code !== 0)
    throw new Error(`signaling Worker is not ready: ${result.stderr}`);
}

async function main(): Promise<void> {
  const requested = process.env.WEBRTC_CASE;
  const matrix = buildWebRtcMatrix();
  if (!requested) {
    console.log(`running ${matrix.length} isolated WebRTC correctness cell(s)`);
    for (const environment of buildIsolatedCaseEnvironments()) {
      const result = await command(
        process.execPath,
        [fileURLToPath(import.meta.url)],
        CELL_PROCESS_TIMEOUT_MS,
        environment,
      );
      process.stdout.write(result.stdout);
      process.stderr.write(result.stderr);
      if (result.code !== 0) {
        throw new Error(
          `isolated WebRTC cell ${environment.WEBRTC_CASE} exited ${result.code}`,
        );
      }
    }
    return;
  }

  await assertSignalReady();
  const cells = requested
    ? matrix.filter((entry) => entry.name === requested)
    : matrix;
  if (requested && cells.length === 0) {
    throw new Error(
      `unknown WEBRTC_CASE=${requested}; choose one of: ${matrix.map((entry) => entry.name).join(", ")}`,
    );
  }
  console.log(`running ${cells.length} WebRTC correctness cell(s)`);
  for (const current of cells) {
    await runCell(current);
    await rm(path.join(RESULTS_DIR, `${current.name}.json`), { force: true });
    await rm(path.join(RESULTS_DIR, "compose-webrtc.log"), { force: true });
  }
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  });
}
