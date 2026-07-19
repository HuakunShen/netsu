import { randomBytes } from "node:crypto";
import { COOKIE_SIZE } from "./states.ts";

const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";

/** 36 random chars from iperf3's cookie alphabet (make_cookie in iperf_util.c). */
export function makeCookie(): string {
  const raw = randomBytes(COOKIE_SIZE - 1);
  let out = "";
  for (const byte of raw) out += ALPHABET[byte % ALPHABET.length];
  return out;
}

/** Wire form: 36 chars + NUL = 37 bytes. */
export function cookieToBytes(cookie: string): Uint8Array {
  const bytes = new Uint8Array(COOKIE_SIZE);
  new TextEncoder().encodeInto(cookie, bytes);
  return bytes;
}

export function bytesToCookie(bytes: Uint8Array): string {
  const end = bytes.indexOf(0);
  return new TextDecoder().decode(bytes.subarray(0, end === -1 ? bytes.length : end));
}
