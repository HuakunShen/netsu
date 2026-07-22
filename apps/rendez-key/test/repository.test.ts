import { env } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";
import {
  claimEntry,
  cleanupEntries,
  createEntry,
} from "../src/repositories/entries";

describe("entries repository", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();
  });

  it("creates and claims an entry once", async () => {
    await createEntry(env.DB, {
      code: "7K3MQ9TX",
      value: "ticket",
      nowSeconds: 1000,
      expiresAtSeconds: 4600,
      maxReads: 1,
    });

    const first = await claimEntry(env.DB, {
      code: "7K3MQ9TX",
      nowSeconds: 1001,
      claimId: "claim-a",
    });

    const second = await claimEntry(env.DB, {
      code: "7K3MQ9TX",
      nowSeconds: 1002,
      claimId: "claim-b",
    });

    expect(first).toEqual({
      value: "ticket",
      remainingReads: 0,
      expiresAtSeconds: 4600,
    });
    expect(second).toBeNull();
  });

  it("rejects expired entries", async () => {
    await createEntry(env.DB, {
      code: "7K3MQ9TX",
      value: "ticket",
      nowSeconds: 1000,
      expiresAtSeconds: 1060,
      maxReads: 1,
    });

    await expect(
      claimEntry(env.DB, {
        code: "7K3MQ9TX",
        nowSeconds: 1060,
        claimId: "claim-a",
      }),
    ).resolves.toBeNull();
  });

  it("cleans expired and exhausted rows", async () => {
    await createEntry(env.DB, {
      code: "7K3MQ9TX",
      value: "expired",
      nowSeconds: 1000,
      expiresAtSeconds: 1010,
      maxReads: 1,
    });

    await createEntry(env.DB, {
      code: "ABCDEFGH",
      value: "active",
      nowSeconds: 1000,
      expiresAtSeconds: 5000,
      maxReads: 1,
    });

    const deleted = await cleanupEntries(env.DB, 2000);
    expect(deleted).toBe(1);
  });
});
