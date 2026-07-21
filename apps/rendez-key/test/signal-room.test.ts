import {
  env,
  runDurableObjectAlarm,
  runInDurableObject,
} from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { SignalRoom } from "../src/signal/room";
import {
  constantTimeDigestEqual,
  decodeBase64Url,
  generateListenerSecret,
  hashListenerSecret,
  hashListenerSecretHex,
  redactListenerSecret,
  verifyListenerSecret,
} from "../src/signal/secret";
import {
  SIGNAL_LISTENER_SECRET_BYTES,
  SIGNAL_LISTENER_SECRET_LENGTH,
  MAX_SIGNAL_CANDIDATES_PER_PEER,
  MAX_SIGNAL_FORWARDED_BYTES,
} from "../src/signal/limits";

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
    this.socket.send(
      typeof message === "string" ? message : JSON.stringify(message),
    );
  }

  async next(): Promise<Record<string, unknown>> {
    const message =
      this.queued.shift() ??
      (await new Promise<string>((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("timed out waiting for WebSocket message")),
          2_000,
        );
        this.waiters.push((value) => {
          clearTimeout(timeout);
          resolve(value);
        });
      }));
    return JSON.parse(message) as Record<string, unknown>;
  }
}

async function createInitializedRoom() {
  const stub = env.SIGNAL_ROOMS.getByName(crypto.randomUUID());
  const secret = generateListenerSecret();
  const expiresAt = Date.now() + 60_000;
  await stub.initialize({
    version: 1,
    listenerSecretHash: await hashListenerSecretHex(secret),
    createdAt: Date.now(),
    expiresAt,
  });
  return { stub, secret, expiresAt };
}

async function openSocket(
  stub: DurableObjectStub<SignalRoom>,
): Promise<SocketInbox> {
  const response = await stub.fetch("https://signal.test/ws", {
    headers: { Upgrade: "websocket" },
  });
  expect(response.status).toBe(101);
  expect(response.webSocket).not.toBeNull();
  return new SocketInbox(response.webSocket!);
}

async function bindPair() {
  const room = await createInitializedRoom();
  const listener = await openSocket(room.stub);
  listener.send({
    v: 1,
    type: "bind",
    role: "listener",
    secret: room.secret,
  });
  expect(await listener.next()).toMatchObject({
    type: "bound",
    role: "listener",
  });

  const joiner = await openSocket(room.stub);
  joiner.send({ v: 1, type: "bind", role: "joiner" });
  expect(await joiner.next()).toMatchObject({ type: "bound", role: "joiner" });
  expect(await listener.next()).toMatchObject({ type: "peer_ready" });
  expect(await joiner.next()).toMatchObject({ type: "peer_ready" });
  return { ...room, listener, joiner };
}

describe("listener secret", () => {
  it("generates an unpadded 256-bit base64url capability", () => {
    const first = generateListenerSecret();
    const second = generateListenerSecret();

    expect(first).toHaveLength(SIGNAL_LISTENER_SECRET_LENGTH);
    expect(first).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(decodeBase64Url(first)).toHaveLength(SIGNAL_LISTENER_SECRET_BYTES);
    expect(second).not.toBe(first);
  });

  it("hashes and compares digests without exposing the clear secret", async () => {
    const secret = generateListenerSecret();
    const digest = await hashListenerSecret(secret);
    const same = await hashListenerSecret(secret);
    const wrong = await hashListenerSecret(generateListenerSecret());
    const hex = await hashListenerSecretHex(secret);

    expect(digest).toHaveLength(32);
    expect(hex).toMatch(/^[0-9a-f]{64}$/);
    expect(constantTimeDigestEqual(digest, same)).toBe(true);
    expect(constantTimeDigestEqual(digest, wrong)).toBe(false);
    expect(await verifyListenerSecret(secret, hex)).toBe(true);
    expect(await verifyListenerSecret(generateListenerSecret(), hex)).toBe(
      false,
    );
    expect(JSON.stringify(redactListenerSecret(secret))).not.toContain(secret);
  });
});

describe("SignalRoom initialization", () => {
  it("persists the first hash and expiry exactly once", async () => {
    const stub = env.SIGNAL_ROOMS.getByName(crypto.randomUUID());
    const firstHash = await hashListenerSecretHex(generateListenerSecret());
    const secondHash = await hashListenerSecretHex(generateListenerSecret());
    const firstExpiry = Date.now() + 60_000;

    const first = await stub.initialize({
      version: 1,
      listenerSecretHash: firstHash,
      createdAt: Date.now(),
      expiresAt: firstExpiry,
    });
    const second = await stub.initialize({
      version: 1,
      listenerSecretHash: secondHash,
      createdAt: Date.now(),
      expiresAt: firstExpiry + 30_000,
    });

    expect(first.created).toBe(true);
    expect(second).toMatchObject({ created: false, expiresAt: firstExpiry });
    expect(await stub.snapshot()).toEqual({
      initialized: true,
      lifecycle: "listener-created",
      expiresAt: firstExpiry,
      listenerSecretHash: "[redacted]",
      terminalReason: null,
    });

    await runInDurableObject(stub, async (_instance, state) => {
      const row = state.storage.sql
        .exec<{
          listener_secret_hash: string;
          expires_at: number;
        }>(
          "SELECT listener_secret_hash, expires_at FROM signal_room WHERE id = 1",
        )
        .one();
      expect(row.listener_secret_hash).toBe(firstHash);
      expect(row.expires_at).toBe(firstExpiry);
      expect(await state.storage.getAlarm()).toBe(firstExpiry);
    });
  });

  it("expires into a non-reusable terminal state via alarm", async () => {
    const stub = env.SIGNAL_ROOMS.getByName(crypto.randomUUID());
    const hash = await hashListenerSecretHex(generateListenerSecret());
    await stub.initialize({
      version: 1,
      listenerSecretHash: hash,
      createdAt: Date.now(),
      expiresAt: Date.now() + 60_000,
    });
    await runInDurableObject(stub, async (_instance, state) => {
      state.storage.sql.exec(
        "UPDATE signal_room SET expires_at = ? WHERE id = 1",
        Date.now() - 1,
      );
    });

    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(await stub.snapshot()).toMatchObject({
      initialized: true,
      lifecycle: "closed",
      listenerSecretHash: "[redacted]",
      terminalReason: "expired",
    });

    const retry = await stub.initialize({
      version: 1,
      listenerSecretHash: await hashListenerSecretHex(generateListenerSecret()),
      createdAt: Date.now(),
      expiresAt: Date.now() + 60_000,
    });
    expect(retry.created).toBe(false);
    expect(retry.lifecycle).toBe("closed");
  });
});

describe("SignalRoom hibernating WebSocket state machine", () => {
  it("rejects non-upgrades and uninitialized rooms", async () => {
    const missing = env.SIGNAL_ROOMS.getByName(crypto.randomUUID());
    const missingResponse = await missing.fetch("https://signal.test/ws", {
      headers: { Upgrade: "websocket" },
    });
    expect(missingResponse.status).toBe(404);

    const { stub } = await createInitializedRoom();
    expect((await stub.fetch("https://signal.test/ws")).status).toBe(426);
  });

  it("authenticates one listener and permits only one joiner", async () => {
    const room = await createInitializedRoom();
    const wrongListener = await openSocket(room.stub);
    wrongListener.send({
      v: 1,
      type: "bind",
      role: "listener",
      secret: generateListenerSecret(),
    });
    expect(await wrongListener.next()).toMatchObject({
      type: "error",
      code: "unauthorized_listener",
    });

    const listener = await openSocket(room.stub);
    listener.send({
      v: 1,
      type: "bind",
      role: "listener",
      secret: room.secret,
    });
    expect(await listener.next()).toMatchObject({ type: "bound" });

    const firstJoiner = await openSocket(room.stub);
    firstJoiner.send({ v: 1, type: "bind", role: "joiner" });
    expect(await firstJoiner.next()).toMatchObject({ type: "bound" });

    const secondJoiner = await openSocket(room.stub);
    secondJoiner.send({ v: 1, type: "bind", role: "joiner" });
    expect(await secondJoiner.next()).toMatchObject({
      type: "error",
      code: "room_full",
    });
  });

  it("forwards offer, answer, candidates, and end markers only to the peer", async () => {
    const { listener, joiner } = await bindPair();
    const offer = {
      v: 1,
      type: "description",
      sdp_type: "offer",
      sdp: "fixture-offer",
    };
    joiner.send(offer);
    expect(await listener.next()).toEqual(offer);

    const answer = {
      v: 1,
      type: "description",
      sdp_type: "answer",
      sdp: "fixture-answer",
    };
    listener.send(answer);
    expect(await joiner.next()).toEqual(answer);

    const candidate = {
      v: 1,
      type: "candidate",
      candidate: "fixture-candidate",
      sdp_mid: "0",
      sdp_mline_index: 0,
      username_fragment: "fixture-fragment",
    };
    joiner.send(candidate);
    expect(await listener.next()).toEqual(candidate);
    joiner.send({ v: 1, type: "end_of_candidates" });
    expect(await listener.next()).toEqual({
      v: 1,
      type: "end_of_candidates",
    });
  });

  it("enforces offer/answer roles and candidate limits", async () => {
    const { listener, joiner } = await bindPair();
    listener.send({
      v: 1,
      type: "description",
      sdp_type: "offer",
      sdp: "wrong-role",
    });
    expect(await listener.next()).toMatchObject({
      type: "error",
      code: "unexpected_message",
    });

    const secondRoom = await bindPair();
    const candidate = {
      v: 1,
      type: "candidate",
      candidate: "fixture-candidate",
      sdp_mid: "0",
      sdp_mline_index: 0,
      username_fragment: "fixture-fragment",
    };
    for (let index = 0; index < MAX_SIGNAL_CANDIDATES_PER_PEER; index += 1) {
      secondRoom.joiner.send(candidate);
      expect(await secondRoom.listener.next()).toEqual(candidate);
    }
    secondRoom.joiner.send(candidate);
    expect(await secondRoom.joiner.next()).toMatchObject({
      type: "error",
      code: "resource_limit",
    });

    joiner.socket.close(1000, "test complete");
  });

  it("enforces total forwarded bytes without retaining payloads", async () => {
    const room = await bindPair();
    await runInDurableObject(room.stub, async (_instance, state) => {
      state.storage.sql.exec(
        "UPDATE signal_room SET forwarded_bytes = ? WHERE id = 1",
        MAX_SIGNAL_FORWARDED_BYTES,
      );
    });
    room.joiner.send({ v: 1, type: "end_of_candidates" });
    expect(await room.joiner.next()).toMatchObject({
      type: "error",
      code: "resource_limit",
    });

    await runInDurableObject(room.stub, async (_instance, state) => {
      const sql = state.storage.sql;
      const table = sql
        .exec<{
          sql: string;
        }>("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'signal_room'")
        .one().sql;
      expect(table).not.toMatch(/sdp|candidate.*text|payload/i);
    });
  });

  it("notifies the peer and makes a left room terminal", async () => {
    const room = await bindPair();
    room.joiner.send({ v: 1, type: "leave" });
    expect(await room.listener.next()).toEqual({ v: 1, type: "peer_left" });
    expect(await room.stub.snapshot()).toMatchObject({
      lifecycle: "closed",
      terminalReason: "peer_left",
    });
  });

  it("closes an unbound socket when its persisted bind deadline expires", async () => {
    const room = await createInitializedRoom();
    const socket = await openSocket(room.stub);
    await runInDurableObject(room.stub, async (_instance, state) => {
      const [server] = state.getWebSockets();
      const attachment = server!.deserializeAttachment() as Record<
        string,
        unknown
      >;
      server!.serializeAttachment({
        ...attachment,
        connectedAt: Date.now() - 10_000,
      });
      await state.storage.setAlarm(Date.now() + 60_000);
    });
    expect(await runDurableObjectAlarm(room.stub)).toBe(true);
    expect(await socket.next()).toMatchObject({
      type: "error",
      code: "unexpected_message",
    });
  });

  it("expires both hibernating sockets idempotently", async () => {
    const room = await bindPair();
    await runInDurableObject(room.stub, async (_instance, state) => {
      state.storage.sql.exec(
        "UPDATE signal_room SET expires_at = ? WHERE id = 1",
        Date.now() - 1,
      );
      await state.storage.setAlarm(Date.now() + 60_000);
    });
    expect(await runDurableObjectAlarm(room.stub)).toBe(true);
    expect(await room.listener.next()).toMatchObject({
      type: "error",
      code: "room_expired",
    });
    expect(await room.joiner.next()).toMatchObject({
      type: "error",
      code: "room_expired",
    });
    expect(await room.stub.snapshot()).toMatchObject({
      lifecycle: "closed",
      terminalReason: "expired",
    });
    expect(await runDurableObjectAlarm(room.stub)).toBe(false);
  });
});
