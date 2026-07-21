import {
  SIGNAL_LISTENER_SECRET_BYTES,
  SIGNAL_LISTENER_SECRET_LENGTH,
} from "./limits";

const textEncoder = new TextEncoder();

function encodeBase64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "");
}

export function decodeBase64Url(value: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new Error("invalid_listener_secret");
  }
  const padding = "=".repeat((4 - (value.length % 4)) % 4);
  let binary: string;
  try {
    binary = atob(value.replace(/-/g, "+").replace(/_/g, "/") + padding);
  } catch {
    throw new Error("invalid_listener_secret");
  }
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

export function generateListenerSecret(): string {
  const bytes = crypto.getRandomValues(
    new Uint8Array(SIGNAL_LISTENER_SECRET_BYTES),
  );
  const secret = encodeBase64Url(bytes);
  if (secret.length !== SIGNAL_LISTENER_SECRET_LENGTH) {
    throw new Error("invalid_listener_secret_generation");
  }
  return secret;
}

export async function hashListenerSecret(secret: string): Promise<Uint8Array> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    textEncoder.encode(secret),
  );
  return new Uint8Array(digest);
}

function bytesToHex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function hexToBytes(value: string): Uint8Array | null {
  if (!/^[0-9a-f]{64}$/.test(value)) return null;
  const bytes = new Uint8Array(32);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

export async function hashListenerSecretHex(secret: string): Promise<string> {
  return bytesToHex(await hashListenerSecret(secret));
}

export function constantTimeDigestEqual(
  left: Uint8Array,
  right: Uint8Array,
): boolean {
  if (left.byteLength !== right.byteLength) return false;
  let difference = 0;
  for (let index = 0; index < left.byteLength; index += 1) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

export async function verifyListenerSecret(
  secret: string,
  expectedHashHex: string,
): Promise<boolean> {
  const expected = hexToBytes(expectedHashHex);
  if (expected === null) return false;
  const actual = await hashListenerSecret(secret);
  return constantTimeDigestEqual(actual, expected);
}

export function redactListenerSecret(_secret: string): {
  listenerSecret: "[redacted]";
} {
  return { listenerSecret: "[redacted]" };
}
