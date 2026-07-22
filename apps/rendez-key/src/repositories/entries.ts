import { and, eq, gt, lte, or, sql } from "drizzle-orm";
import { drizzle } from "drizzle-orm/d1";
import { entries } from "../db/schema";

export interface CreateEntryInput {
  code: string;
  value: string;
  nowSeconds: number;
  expiresAtSeconds: number;
  maxReads: number;
}

export interface StoredEntry {
  code: string;
  expiresAtSeconds: number;
  maxReads: number;
}

export interface ClaimEntryInput {
  code: string;
  nowSeconds: number;
  claimId: string;
}

export interface ClaimedEntry {
  value: string;
  remainingReads: number;
  expiresAtSeconds: number;
}

export async function createEntry(
  db: D1Database,
  input: CreateEntryInput,
): Promise<StoredEntry> {
  const orm = drizzle(db);

  await orm.insert(entries).values({
    code: input.code,
    value: input.value,
    createdAt: input.nowSeconds,
    expiresAt: input.expiresAtSeconds,
    maxReads: input.maxReads,
    remainingReads: input.maxReads,
    lastClaimId: null,
  });

  return {
    code: input.code,
    expiresAtSeconds: input.expiresAtSeconds,
    maxReads: input.maxReads,
  };
}

export async function claimEntry(
  db: D1Database,
  input: ClaimEntryInput,
): Promise<ClaimedEntry | null> {
  const orm = drizzle(db);

  const [updateResult, selectResult] = await orm.batch([
    orm
      .update(entries)
      .set({
        remainingReads: sql`${entries.remainingReads} - 1`,
        lastClaimId: input.claimId,
      })
      .where(
        and(
          eq(entries.code, input.code),
          gt(entries.expiresAt, input.nowSeconds),
          gt(entries.remainingReads, 0),
        ),
      ),
    orm
      .select({
        value: entries.value,
        remainingReads: entries.remainingReads,
        expiresAt: entries.expiresAt,
      })
      .from(entries)
      .where(
        and(eq(entries.code, input.code), eq(entries.lastClaimId, input.claimId)),
      )
      .limit(1),
  ]);

  if (!updateResult.success) {
    throw new Error("claim_update_failed");
  }

  const row = selectResult[0];

  if (row === undefined) {
    return null;
  }

  return {
    value: row.value,
    remainingReads: row.remainingReads,
    expiresAtSeconds: row.expiresAt,
  };
}

export async function cleanupEntries(
  db: D1Database,
  nowSeconds: number,
): Promise<number> {
  const orm = drizzle(db);

  // Delete every stale row in a single statement. Correctness never depends on
  // this running (claims re-validate expiry/reads at request time), so there is
  // no need to batch — this only reclaims D1 storage.
  const result = await orm
    .delete(entries)
    .where(
      or(lte(entries.expiresAt, nowSeconds), lte(entries.remainingReads, 0)),
    );

  return result.meta.changes;
}
