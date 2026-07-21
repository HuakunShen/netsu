import { sql } from "drizzle-orm";
import { check, index, integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

export const entries = sqliteTable(
  "entries",
  {
    code: text("code").primaryKey(),
    value: text("value").notNull(),
    createdAt: integer("created_at").notNull(),
    expiresAt: integer("expires_at").notNull(),
    maxReads: integer("max_reads").notNull(),
    remainingReads: integer("remaining_reads").notNull(),
    lastClaimId: text("last_claim_id"),
  },
  (table) => [
    check("max_reads_range", sql`${table.maxReads} BETWEEN 1 AND 100`),
    check("remaining_reads_non_negative", sql`${table.remainingReads} >= 0`),
    index("idx_entries_cleanup").on(table.expiresAt, table.remainingReads),
  ],
);
