import {
  env,
  runDurableObjectAlarm,
  runInDurableObject,
} from "cloudflare:test";
import { describe, expect, it } from "vitest";
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
} from "../src/signal/limits";

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
        }>("SELECT listener_secret_hash, expires_at FROM signal_room WHERE id = 1")
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
