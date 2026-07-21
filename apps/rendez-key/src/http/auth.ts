import type { Context, MiddlewareHandler } from "hono";
import { problem } from "./errors";

/**
 * Which limit profile a create request runs under. `privileged` is granted by a
 * valid API token; `anonymous` is granted to unauthenticated callers only when
 * open mode is enabled, and runs under tighter caps plus per-IP rate limiting.
 */
export type CreateTier = "privileged" | "anonymous";

export interface CreateAuthVariables {
  createTier: CreateTier;
}

const encoder = new TextEncoder();

function timingSafeStringEqual(left: string, right: string): boolean {
  const leftBytes = encoder.encode(left);
  const rightBytes = encoder.encode(right);

  if (leftBytes.byteLength !== rightBytes.byteLength) {
    return false;
  }

  return crypto.subtle.timingSafeEqual(leftBytes, rightBytes);
}

/**
 * Open mode is opt-in via the `PUBLIC_CREATE` variable. It is read as a string
 * (Cloudflare vars are strings) and normalized so `"true"`/`"1"` enable it and
 * anything else — including an unset variable — leaves creation token-only.
 */
function isOpenMode(env: CloudflareBindings): boolean {
  const value = String(env.PUBLIC_CREATE ?? "").trim().toLowerCase();
  return value === "true" || value === "1";
}

function bearerToken(header: string): string | null {
  const match = /^Bearer\s+(.+)$/i.exec(header.trim());
  return match?.[1] ?? null;
}

function clientIp(c: Context): string {
  return c.req.header("CF-Connecting-IP") ?? "unknown";
}

/**
 * Returns true when the anonymous caller has exceeded the per-IP create limit.
 * Fails open when no rate-limit binding is configured (e.g. a deployment that
 * enabled open mode but omitted the binding, or a local run without it) — the
 * tightened anonymous caps still bound the blast radius.
 */
async function isRateLimited(
  env: CloudflareBindings,
  key: string,
): Promise<boolean> {
  const limiter = env.CREATE_LIMITER;
  if (!limiter) {
    return false;
  }

  const { success } = await limiter.limit({ key });
  return !success;
}

export const authorizeCreate: MiddlewareHandler<{
  Bindings: CloudflareBindings;
  Variables: CreateAuthVariables;
}> = async (c, next) => {
  const authHeader = c.req.header("Authorization");
  const configuredToken = c.env.API_TOKEN;

  // Any Authorization header means the caller is asking for the privileged
  // tier. It must be a valid token or we reject with 401 — we never silently
  // downgrade a token-bearing request to the anonymous tier, since its
  // higher-cap request would then fail confusingly on limits, not on auth.
  if (authHeader !== undefined) {
    const token = bearerToken(authHeader);

    if (
      token !== null &&
      typeof configuredToken === "string" &&
      configuredToken.length > 0 &&
      timingSafeStringEqual(token, configuredToken)
    ) {
      c.set("createTier", "privileged");
      await next();
      return;
    }

    return problem(c, 401, "unauthorized", "Unauthorized");
  }

  // No Authorization header: allowed only in open mode.
  if (!isOpenMode(c.env)) {
    return problem(c, 401, "unauthorized", "Unauthorized");
  }

  if (await isRateLimited(c.env, clientIp(c))) {
    return problem(c, 429, "rate_limited", "Too Many Requests");
  }

  c.set("createTier", "anonymous");
  await next();
};
