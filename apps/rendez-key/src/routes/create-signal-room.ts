import type { Handler } from "hono";
import { z } from "zod";
import { formatCode, generateCode } from "../domain/code";
import { problem } from "../http/errors";
import type { SignalCreateAuthVariables } from "../http/signal-auth";
import {
  DEFAULT_SIGNAL_TTL_SECONDS,
  MAX_SIGNAL_TTL_SECONDS,
  MIN_SIGNAL_TTL_SECONDS,
  SIGNAL_PROTOCOL_VERSION,
} from "../signal/limits";
import {
  generateListenerSecret,
  hashListenerSecretHex,
} from "../signal/secret";

const requestSchema = z.strictObject({
  v: z.literal(SIGNAL_PROTOCOL_VERSION),
  ttl_seconds: z
    .number()
    .int()
    .min(MIN_SIGNAL_TTL_SECONDS)
    .max(MAX_SIGNAL_TTL_SECONDS)
    .default(DEFAULT_SIGNAL_TTL_SECONDS),
});

const MAX_CODE_ATTEMPTS = 5;

export interface SignalRoomAllocation {
  code: string;
  listenerSecret: string;
  expiresAt: number;
}

export async function allocateSignalRoom(
  env: Pick<CloudflareBindings, "SIGNAL_ROOMS">,
  ttlSeconds: number,
  dependencies: {
    code?: () => string;
    secret?: () => string;
    now?: () => number;
  } = {},
): Promise<SignalRoomAllocation | null> {
  const createdAt = (dependencies.now ?? Date.now)();
  const expiresAt = createdAt + ttlSeconds * 1_000;
  const listenerSecret = (dependencies.secret ?? generateListenerSecret)();
  const listenerSecretHash = await hashListenerSecretHex(listenerSecret);
  const nextCode = dependencies.code ?? generateCode;

  for (let attempt = 0; attempt < MAX_CODE_ATTEMPTS; attempt += 1) {
    const code = nextCode();
    const stub = env.SIGNAL_ROOMS.getByName(code);
    const input = {
      version: SIGNAL_PROTOCOL_VERSION,
      listenerSecretHash,
      createdAt,
      expiresAt,
    } as const;
    let result;
    try {
      result = await stub.initialize(input);
    } catch {
      // An RPC can fail after its storage output gate committed. Retrying the
      // same id/input lets initialize report whether this secret owns it.
      try {
        result = await stub.initialize(input);
      } catch {
        continue;
      }
    }
    if (result.created || result.matchesInput) {
      return { code, listenerSecret, expiresAt };
    }
  }
  return null;
}

export const createSignalRoomRoute: Handler<{
  Bindings: CloudflareBindings;
  Variables: SignalCreateAuthVariables;
}> = async (c) => {
  let body: unknown;
  try {
    body = await c.req.json();
  } catch {
    return problem(c, 400, "invalid_request", "Invalid JSON request");
  }
  const parsed = requestSchema.safeParse(body);
  if (!parsed.success) {
    return problem(c, 400, "invalid_request", "Invalid signaling room request");
  }

  const allocation = await allocateSignalRoom(c.env, parsed.data.ttl_seconds);
  if (allocation !== null) {
    console.log(JSON.stringify({ event: "signal_room_created", count: 1 }));
    return c.json(
      {
        v: SIGNAL_PROTOCOL_VERSION,
        code: formatCode(allocation.code),
        listener_secret: allocation.listenerSecret,
        expires_at: new Date(allocation.expiresAt).toISOString(),
      },
      201,
    );
  }

  return problem(
    c,
    503,
    "code_generation_failed",
    "Unable to allocate signaling room",
  );
};
