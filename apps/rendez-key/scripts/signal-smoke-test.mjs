#!/usr/bin/env bun
import { writeFile } from "node:fs/promises";

const baseUrl = process.argv[2];
if (!baseUrl) {
  throw new Error("usage: signal-smoke-test.mjs <http(s)://host/v1/signal>");
}

const iterations = Number.parseInt(
  process.env.SIGNAL_SMOKE_ITERATIONS ?? "1",
  10,
);
if (!Number.isSafeInteger(iterations) || iterations < 1 || iterations > 100) {
  throw new Error("SIGNAL_SMOKE_ITERATIONS must be 1..100");
}

const token = process.env.RENDEZKEY_TOKEN;
const sentinels = [];

class Inbox {
  #queued = [];
  #waiters = [];

  constructor(socket) {
    this.socket = socket;
    socket.addEventListener("message", (event) => {
      const waiter = this.#waiters.shift();
      if (waiter) waiter.resolve(String(event.data));
      else this.#queued.push(String(event.data));
    });
  }

  send(message) {
    this.socket.send(JSON.stringify(message));
  }

  async next() {
    const queued = this.#queued.shift();
    if (queued !== undefined) return JSON.parse(queued);
    const raw = await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        const index = this.#waiters.findIndex(
          (entry) => entry.resolve === resolve,
        );
        if (index >= 0) this.#waiters.splice(index, 1);
        reject(new Error("timed out waiting for signaling frame"));
      }, 5_000);
      this.#waiters.push({
        resolve: (value) => {
          clearTimeout(timeout);
          resolve(value);
        },
      });
    });
    return JSON.parse(raw);
  }
}

function assertSubset(actual, expected, label) {
  for (const [key, value] of Object.entries(expected)) {
    if (actual?.[key] !== value) {
      throw new Error(
        `${label}: expected ${key}=${value}, got ${actual?.[key]}`,
      );
    }
  }
}

function websocketUrl(code) {
  const url = new URL(`${baseUrl}/rooms/${code}/ws`);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.href;
}

async function openSocket(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(
      () => reject(new Error("WebSocket open timed out")),
      5_000,
    );
    socket.addEventListener(
      "open",
      () => {
        clearTimeout(timeout);
        resolve();
      },
      { once: true },
    );
    socket.addEventListener(
      "error",
      () => {
        clearTimeout(timeout);
        reject(new Error("WebSocket upgrade failed"));
      },
      { once: true },
    );
  });
  return new Inbox(socket);
}

async function expectUpgradeRejected(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error("terminal room unexpectedly kept upgrade pending"));
    }, 3_000);
    socket.addEventListener(
      "open",
      () => {
        clearTimeout(timeout);
        socket.close();
        reject(new Error("terminal room was reusable"));
      },
      { once: true },
    );
    const rejected = () => {
      clearTimeout(timeout);
      resolve();
    };
    socket.addEventListener("error", rejected, { once: true });
    socket.addEventListener("close", rejected, { once: true });
  });
}

async function runOne(index) {
  const response = await fetch(`${baseUrl}/rooms`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify({ v: 1, ttl_seconds: 60 }),
  });
  if (response.status !== 201) {
    throw new Error(
      `room create failed: ${response.status} ${await response.text()}`,
    );
  }
  const room = await response.json();
  sentinels.push(room.listener_secret);

  const listener = await openSocket(websocketUrl(room.code));
  listener.send({
    v: 1,
    type: "bind",
    role: "listener",
    secret: room.listener_secret,
  });
  assertSubset(
    await listener.next(),
    { type: "bound", role: "listener" },
    "listener bind",
  );

  const joiner = await openSocket(websocketUrl(room.code));
  joiner.send({ v: 1, type: "bind", role: "joiner" });
  assertSubset(
    await joiner.next(),
    { type: "bound", role: "joiner" },
    "joiner bind",
  );
  assertSubset(await listener.next(), { type: "peer_ready" }, "listener ready");
  assertSubset(await joiner.next(), { type: "peer_ready" }, "joiner ready");

  const suffix = `${index}-${crypto.randomUUID()}`;
  const offer = `signal-smoke-offer-${suffix}`;
  const answer = `signal-smoke-answer-${suffix}`;
  const candidate = `signal-smoke-candidate-${suffix}`;
  sentinels.push(offer, answer, candidate);

  joiner.send({ v: 1, type: "description", sdp_type: "offer", sdp: offer });
  assertSubset(
    await listener.next(),
    { type: "description", sdp: offer },
    "offer forward",
  );
  listener.send({ v: 1, type: "description", sdp_type: "answer", sdp: answer });
  assertSubset(
    await joiner.next(),
    { type: "description", sdp: answer },
    "answer forward",
  );
  joiner.send({
    v: 1,
    type: "candidate",
    candidate,
    sdp_mid: "0",
    sdp_mline_index: 0,
    username_fragment: `fragment-${index}`,
  });
  assertSubset(
    await listener.next(),
    { type: "candidate", candidate },
    "candidate forward",
  );

  joiner.send({ v: 1, type: "leave" });
  assertSubset(
    await listener.next(),
    { type: "peer_left" },
    "peer-left notification",
  );
  await expectUpgradeRejected(websocketUrl(room.code));
}

for (let index = 0; index < iterations; index += 1) {
  await runOne(index);
}

if (process.env.SIGNAL_SMOKE_SENTINELS) {
  await writeFile(
    process.env.SIGNAL_SMOKE_SENTINELS,
    `${sentinels.join("\n")}\n`,
  );
}

console.log(
  `signaling smoke passed (${iterations} room${iterations === 1 ? "" : "s"})`,
);
