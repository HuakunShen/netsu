import type { Context } from "hono";
import { normalizeCode } from "../domain/code";
import { problem } from "../http/errors";
import { claimEntry } from "../repositories/entries";

function epochSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

function unavailable(c: Context) {
  return problem(
    c,
    404,
    "entry_not_available",
    "Entry not available",
  );
}

export async function claimEntryRoute(
  c: Context<
    { Bindings: CloudflareBindings },
    "/v1/entries/:code/claim"
  >,
) {
  const normalizedCode = normalizeCode(c.req.param("code"));

  if (normalizedCode === null) {
    return unavailable(c);
  }

  const claimed = await claimEntry(c.env.DB, {
    code: normalizedCode,
    nowSeconds: epochSeconds(),
    claimId: crypto.randomUUID(),
  });

  if (claimed === null) {
    console.log(
      JSON.stringify({
        event: "entry_claim_unavailable",
        status: 404,
      }),
    );
    return unavailable(c);
  }

  const expiresAt = new Date(
    claimed.expiresAtSeconds * 1000,
  ).toISOString();

  console.log(
    JSON.stringify({
      event: "entry_claimed",
      remaining_reads: claimed.remainingReads,
      status: 200,
    }),
  );

  c.header("Content-Type", "text/plain; charset=utf-8");
  c.header(
    "X-RendezKey-Remaining-Reads",
    String(claimed.remainingReads),
  );
  c.header("X-RendezKey-Expires-At", expiresAt);

  return c.body(claimed.value, 200);
}
