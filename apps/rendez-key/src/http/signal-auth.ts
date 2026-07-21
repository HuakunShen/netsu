import type { Context, MiddlewareHandler } from "hono";
import { problem } from "./errors";

export interface SignalCreateAuthVariables {
  signalCreateTier: "privileged" | "anonymous";
}

const encoder = new TextEncoder();

function timingSafeStringEqual(left: string, right: string): boolean {
  const leftBytes = encoder.encode(left);
  const rightBytes = encoder.encode(right);
  if (leftBytes.byteLength !== rightBytes.byteLength) return false;
  return crypto.subtle.timingSafeEqual(leftBytes, rightBytes);
}

function bearerToken(header: string): string | null {
  return /^Bearer\s+(.+)$/i.exec(header.trim())?.[1] ?? null;
}

function publicSignalCreateEnabled(env: CloudflareBindings): boolean {
  const value = String(env.PUBLIC_SIGNAL_CREATE ?? "")
    .trim()
    .toLowerCase();
  return value === "true" || value === "1";
}

function clientIp(c: Context): string {
  return c.req.header("CF-Connecting-IP") ?? "unknown";
}

export const authorizeSignalCreate: MiddlewareHandler<{
  Bindings: CloudflareBindings;
  Variables: SignalCreateAuthVariables;
}> = async (c, next) => {
  const authHeader = c.req.header("Authorization");
  if (authHeader !== undefined) {
    const token = bearerToken(authHeader);
    const configured = c.env.API_TOKEN;
    if (
      token !== null &&
      typeof configured === "string" &&
      configured.length > 0 &&
      timingSafeStringEqual(token, configured)
    ) {
      c.set("signalCreateTier", "privileged");
      await next();
      return;
    }
    return problem(c, 401, "unauthorized", "Unauthorized");
  }

  if (!publicSignalCreateEnabled(c.env)) {
    return problem(
      c,
      403,
      "signal_create_disabled",
      "Anonymous signaling room creation is disabled",
    );
  }

  const limiter = c.env.SIGNAL_CREATE_LIMITER;
  if (limiter) {
    const { success } = await limiter.limit({ key: clientIp(c) });
    if (!success) {
      return problem(c, 429, "rate_limited", "Too Many Requests");
    }
  }

  c.set("signalCreateTier", "anonymous");
  await next();
};
