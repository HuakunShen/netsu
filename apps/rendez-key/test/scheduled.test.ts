import { env } from "cloudflare:workers";
import { beforeEach, describe, expect, it } from "vitest";
import { runCleanup } from "../src/scheduled";

describe("scheduled cleanup", () => {
  beforeEach(async () => {
    await env.DB.prepare("DELETE FROM entries").run();

    await env.DB.batch([
      env.DB
        .prepare(
          `INSERT INTO entries
           (code, value, created_at, expires_at, max_reads, remaining_reads)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
        )
        .bind("ABCDEFGH", "expired", 1000, 1100, 1, 1),
      env.DB
        .prepare(
          `INSERT INTO entries
           (code, value, created_at, expires_at, max_reads, remaining_reads)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
        )
        .bind("JKLMNPQR", "exhausted", 1000, 5000, 1, 0),
      env.DB
        .prepare(
          `INSERT INTO entries
           (code, value, created_at, expires_at, max_reads, remaining_reads)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
        )
        .bind("STUVWXYZ", "active", 1000, 5000, 1, 1),
    ]);
  });

  it("deletes expired and exhausted entries only", async () => {
    await runCleanup(env, 2_000_000);

    const rows = await env.DB.prepare(
      "SELECT code FROM entries ORDER BY code",
    ).all<{ code: string }>();

    expect(rows.results).toEqual([{ code: "STUVWXYZ" }]);
  });
});
