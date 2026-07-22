import { SELF, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { createApp } from "../src/app";
import { allocateSignalRoom } from "../src/routes/create-signal-room";
import {
  generateListenerSecret,
  hashListenerSecretHex,
} from "../src/signal/secret";

class SocketInbox {
  private readonly queued: string[] = [];
  private readonly waiters: Array<(message: string) => void> = [];

  constructor(readonly socket: WebSocket) {
    socket.accept();
    socket.addEventListener("message", (event) => {
      const message = String(event.data);
      const waiter = this.waiters.shift();
      if (waiter) waiter(message);
      else this.queued.push(message);
    });
  }

  send(message: unknown): void {
    this.socket.send(JSON.stringify(message));
  }

  async next(): Promise<Record<string, unknown>> {
    const raw =
      this.queued.shift() ??
      (await new Promise<string>((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("timed out waiting for signaling message")),
          2_000,
        );
        this.waiters.push((value) => {
          clearTimeout(timeout);
          resolve(value);
        });
      }));
    return JSON.parse(raw) as Record<string, unknown>;
  }
}

async function createRoom(body: unknown = { v: 1, ttl_seconds: 600 }): Promise<{
  code: string;
  listener_secret: string;
  expires_at: string;
}> {
  const response = await SELF.fetch("https://example.test/v1/signal/rooms", {
    method: "POST",
    headers: {
      Authorization: "Bearer test-token",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  expect(response.status).toBe(201);
  return response.json();
}

async function connect(code: string): Promise<SocketInbox> {
  const response = await SELF.fetch(
    `https://example.test/v1/signal/rooms/${code}/ws`,
    { headers: { Upgrade: "websocket" } },
  );
  expect(response.status).toBe(101);
  return new SocketInbox(response.webSocket!);
}

describe("POST /v1/signal/rooms", () => {
  it("allows rate-limited anonymous room creation in the production config", async () => {
    const response = await SELF.fetch("https://example.test/v1/signal/rooms", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ v: 1, ttl_seconds: 600 }),
    });
    expect(response.status).toBe(201);
    expect(await response.json()).toMatchObject({
      code: expect.stringMatching(/^[23456789A-HJ-NP-Z]{4}-[23456789A-HJ-NP-Z]{4}$/),
      listener_secret: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/),
    });
  });

  it("can disable anonymous room creation explicitly", async () => {
    const { app } = createApp();
    const response = await app.request(
      "https://example.test/v1/signal/rooms",
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ v: 1, ttl_seconds: 600 }),
      },
      {
        ...env,
        PUBLIC_SIGNAL_CREATE: "false",
      } as unknown as CloudflareBindings,
    );
    expect(response.status).toBe(403);
    expect(await response.json()).toMatchObject({ code: "signal_create_disabled" });
  });

  it("rejects invalid tokens and malformed TTLs", async () => {
    const invalidToken = await SELF.fetch(
      "https://example.test/v1/signal/rooms",
      {
        method: "POST",
        headers: {
          Authorization: "Bearer wrong",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ v: 1, ttl_seconds: 600 }),
      },
    );
    expect(invalidToken.status).toBe(401);

    const invalidTtl = await SELF.fetch(
      "https://example.test/v1/signal/rooms",
      {
        method: "POST",
        headers: {
          Authorization: "Bearer test-token",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ v: 1, ttl_seconds: 59 }),
      },
    );
    expect(invalidTtl.status).toBe(400);
  });

  it("returns a formatted code, one-time secret, and bounded expiry", async () => {
    const before = Date.now();
    const room = await createRoom();
    const expiry = Date.parse(room.expires_at);

    expect(room.code).toMatch(
      /^[23456789A-HJ-NP-Z]{4}-[23456789A-HJ-NP-Z]{4}$/,
    );
    expect(room.listener_secret).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(expiry).toBeGreaterThanOrEqual(before + 599_000);
    expect(expiry).toBeLessThanOrEqual(Date.now() + 601_000);
  });

  it("uses a dedicated anonymous signaling limiter", async () => {
    const { app } = createApp();
    const response = await app.request(
      "https://example.test/v1/signal/rooms",
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "CF-Connecting-IP": "192.0.2.10",
        },
        body: JSON.stringify({ v: 1, ttl_seconds: 600 }),
      },
      {
        ...env,
        PUBLIC_SIGNAL_CREATE: "true",
        SIGNAL_CREATE_LIMITER: {
          limit: async ({ key }: { key: string }) => {
            expect(key).toBe("192.0.2.10");
            return { success: false };
          },
        },
      } as unknown as CloudflareBindings,
    );
    expect(response.status).toBe(429);
    expect(await response.json()).toMatchObject({ code: "rate_limited" });
  });

  it("retries a colliding room code without overwriting its owner", async () => {
    const collisionCode = "22222222";
    const availableCode = "33333333";
    const existing = env.SIGNAL_ROOMS.getByName(collisionCode);
    const existingHash = await hashListenerSecretHex(generateListenerSecret());
    await existing.initialize({
      version: 1,
      listenerSecretHash: existingHash,
      createdAt: Date.now(),
      expiresAt: Date.now() + 60_000,
    });
    const codes = [collisionCode, availableCode];
    const allocation = await allocateSignalRoom(env, 600, {
      code: () => codes.shift() ?? availableCode,
    });

    expect(allocation?.code).toBe(availableCode);
    await runInDurableObject(existing, async (_instance, state) => {
      const stored = state.storage.sql
        .exec<{
          listener_secret_hash: string;
        }>("SELECT listener_secret_hash FROM signal_room WHERE id = 1")
        .one();
      expect(stored.listener_secret_hash).toBe(existingHash);
    });
  });
});

describe("GET /v1/signal/rooms/:code/ws", () => {
  it("normalizes codes and rejects missing rooms/non-upgrades", async () => {
    const room = await createRoom();
    const normalized = room.code.replace("-", "").toLowerCase();
    const nonUpgrade = await SELF.fetch(
      `https://example.test/v1/signal/rooms/${normalized}/ws`,
    );
    expect(nonUpgrade.status).toBe(426);

    const missing = await SELF.fetch(
      "https://example.test/v1/signal/rooms/ZZZZ-ZZZZ/ws",
      { headers: { Upgrade: "websocket" } },
    );
    expect(missing.status).toBe(404);
  });

  it("carries a real two-socket offer/answer exchange through the Worker", async () => {
    const room = await createRoom();
    const listener = await connect(room.code);
    listener.send({
      v: 1,
      type: "bind",
      role: "listener",
      secret: room.listener_secret,
    });
    expect(await listener.next()).toMatchObject({ type: "bound" });

    const joiner = await connect(room.code);
    joiner.send({ v: 1, type: "bind", role: "joiner" });
    expect(await joiner.next()).toMatchObject({ type: "bound" });
    expect(await listener.next()).toMatchObject({ type: "peer_ready" });
    expect(await joiner.next()).toMatchObject({ type: "peer_ready" });

    joiner.send({
      v: 1,
      type: "description",
      sdp_type: "offer",
      sdp: "route-fixture-offer",
    });
    expect(await listener.next()).toMatchObject({
      type: "description",
      sdp_type: "offer",
      sdp: "route-fixture-offer",
    });
    listener.send({
      v: 1,
      type: "description",
      sdp_type: "answer",
      sdp: "route-fixture-answer",
    });
    expect(await joiner.next()).toMatchObject({
      type: "description",
      sdp_type: "answer",
      sdp: "route-fixture-answer",
    });
  });
});
