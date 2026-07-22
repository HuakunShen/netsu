import type { Context } from "hono";
import { formatCode, generateCode } from "../domain/code";
import {
  ANONYMOUS_LIMITS,
  PRIVILEGED_LIMITS,
  parseMaxReads,
  parseTtl,
  readUtf8Body,
} from "../domain/limits";
import type { CreateAuthVariables } from "../http/auth";
import { problem } from "../http/errors";
import { createEntry } from "../repositories/entries";

const MAX_CODE_INSERT_ATTEMPTS = 5;

function epochSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

function isUniqueConstraintError(error: unknown): boolean {
  return (
    error instanceof Error &&
    error.message.toLowerCase().includes("unique")
  );
}

export async function createEntryRoute(
  c: Context<{
    Bindings: CloudflareBindings;
    Variables: CreateAuthVariables;
  }>,
) {
  const contentType = c.req.header("Content-Type") ?? "";

  if (!contentType.toLowerCase().startsWith("text/plain")) {
    return problem(
      c,
      400,
      "invalid_request",
      "Invalid request",
      "Content-Type must be text/plain",
    );
  }

  const tier = c.get("createTier");
  const limits = tier === "anonymous" ? ANONYMOUS_LIMITS : PRIVILEGED_LIMITS;

  let ttlSeconds: number;
  let maxReads: number;
  let value: string;

  try {
    ttlSeconds = parseTtl(c.req.query("ttl"), limits);
    maxReads = parseMaxReads(c.req.query("reads"), limits);
    value = await readUtf8Body(c.req.raw, limits);
  } catch (error) {
    const code = error instanceof Error ? error.message : "invalid_request";

    if (code === "payload_too_large") {
      return problem(
        c,
        413,
        "payload_too_large",
        "Payload too large",
      );
    }

    return problem(
      c,
      400,
      "invalid_request",
      "Invalid request",
      code,
    );
  }

  const nowSeconds = epochSeconds();
  const expiresAtSeconds = nowSeconds + ttlSeconds;

  for (
    let attempt = 0;
    attempt < MAX_CODE_INSERT_ATTEMPTS;
    attempt += 1
  ) {
    const normalizedCode = generateCode();

    try {
      await createEntry(c.env.DB, {
        code: normalizedCode,
        value,
        nowSeconds,
        expiresAtSeconds,
        maxReads,
      });

      const displayCode = formatCode(normalizedCode);
      const expiresAt = new Date(expiresAtSeconds * 1000).toISOString();

      console.log(
        JSON.stringify({
          event: "entry_created",
          tier,
          payload_bytes: new TextEncoder().encode(value).byteLength,
          ttl_seconds: ttlSeconds,
          max_reads: maxReads,
          status: 201,
        }),
      );

      if ((c.req.header("Accept") ?? "").includes("text/plain")) {
        c.header("Content-Type", "text/plain; charset=utf-8");
        c.header("X-RendezKey-Expires-At", expiresAt);
        c.header("X-RendezKey-Max-Reads", String(maxReads));
        return c.body(displayCode, 201);
      }

      return c.json(
        {
          code: displayCode,
          expires_at: expiresAt,
          max_reads: maxReads,
        },
        201,
      );
    } catch (error) {
      if (isUniqueConstraintError(error)) {
        continue;
      }

      throw error;
    }
  }

  return problem(
    c,
    503,
    "code_generation_failed",
    "Code generation failed",
  );
}
