export const DEFAULT_TTL_SECONDS = 3_600;
export const MIN_TTL_SECONDS = 60;
export const MAX_TTL_SECONDS = 604_800;

export const DEFAULT_MAX_READS = 1;
export const MIN_MAX_READS = 1;
export const MAX_MAX_READS = 100;

export const MAX_PAYLOAD_BYTES = 65_536;

/**
 * A set of bounds applied to a single create request. The privileged (token)
 * tier keeps the historical, generous ceilings; the anonymous (open) tier
 * tightens the maximums a caller can request to bound abuse. Defaults and
 * minimums are identical across tiers — only the ceilings differ.
 */
export interface LimitProfile {
  defaultTtlSeconds: number;
  minTtlSeconds: number;
  maxTtlSeconds: number;
  defaultMaxReads: number;
  minMaxReads: number;
  maxMaxReads: number;
  maxPayloadBytes: number;
}

export const PRIVILEGED_LIMITS: LimitProfile = {
  defaultTtlSeconds: DEFAULT_TTL_SECONDS,
  minTtlSeconds: MIN_TTL_SECONDS,
  maxTtlSeconds: MAX_TTL_SECONDS,
  defaultMaxReads: DEFAULT_MAX_READS,
  minMaxReads: MIN_MAX_READS,
  maxMaxReads: MAX_MAX_READS,
  maxPayloadBytes: MAX_PAYLOAD_BYTES,
};

export const ANONYMOUS_LIMITS: LimitProfile = {
  defaultTtlSeconds: DEFAULT_TTL_SECONDS,
  minTtlSeconds: MIN_TTL_SECONDS,
  maxTtlSeconds: 3_600,
  defaultMaxReads: DEFAULT_MAX_READS,
  minMaxReads: MIN_MAX_READS,
  maxMaxReads: 5,
  maxPayloadBytes: 8_192,
};

function parseBoundedInteger(
  raw: string | undefined,
  defaultValue: number,
  minimum: number,
  maximum: number,
  errorCode: string,
): number {
  if (raw === undefined) {
    return defaultValue;
  }

  if (!/^\d+$/.test(raw)) {
    throw new Error(errorCode);
  }

  const value = Number(raw);

  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(errorCode);
  }

  return value;
}

export function parseTtl(
  raw: string | undefined,
  profile: LimitProfile = PRIVILEGED_LIMITS,
): number {
  return parseBoundedInteger(
    raw,
    profile.defaultTtlSeconds,
    profile.minTtlSeconds,
    profile.maxTtlSeconds,
    "invalid_ttl",
  );
}

export function parseMaxReads(
  raw: string | undefined,
  profile: LimitProfile = PRIVILEGED_LIMITS,
): number {
  return parseBoundedInteger(
    raw,
    profile.defaultMaxReads,
    profile.minMaxReads,
    profile.maxMaxReads,
    "invalid_reads",
  );
}

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

export async function readUtf8Body(
  request: Request,
  profile: LimitProfile = PRIVILEGED_LIMITS,
): Promise<string> {
  const maxPayloadBytes = profile.maxPayloadBytes;
  const declaredLength = request.headers.get("content-length");

  if (
    declaredLength !== null &&
    Number.isFinite(Number(declaredLength)) &&
    Number(declaredLength) > maxPayloadBytes
  ) {
    throw new Error("payload_too_large");
  }

  const value = await request.text();
  const byteLength = utf8ByteLength(value);

  if (byteLength < 1) {
    throw new Error("empty_payload");
  }

  if (byteLength > maxPayloadBytes) {
    throw new Error("payload_too_large");
  }

  return value;
}
