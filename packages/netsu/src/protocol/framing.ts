import type { BytePipe } from "./pipe.ts";

/** Single signed state byte (iperf3 writes states as one byte on the control channel). */
export async function writeState(pipe: BytePipe, state: number): Promise<void> {
  await pipe.write(new Uint8Array([state & 0xff]));
}

export async function readState(pipe: BytePipe, timeoutMs?: number): Promise<number> {
  const b = await pipe.readExact(1, timeoutMs);
  return (b[0]! << 24) >> 24; // sign-extend
}

/** [u32 BE length][UTF-8 JSON] — JSON_write in iperf_api.c. */
export async function writeJson(pipe: BytePipe, value: unknown): Promise<void> {
  const body = new TextEncoder().encode(JSON.stringify(value));
  const frame = new Uint8Array(4 + body.length);
  new DataView(frame.buffer).setUint32(0, body.length);
  frame.set(body, 4);
  await pipe.write(frame);
}

export async function readJson(
  pipe: BytePipe,
  maxSize = 65536,
  timeoutMs?: number,
): Promise<unknown> {
  const head = await pipe.readExact(4, timeoutMs);
  const size = new DataView(head.buffer, head.byteOffset).getUint32(0);
  if (size === 0 || size > maxSize) {
    throw new Error(`json frame too large: ${size} > ${maxSize}`);
  }
  const body = await pipe.readExact(size, timeoutMs);
  return JSON.parse(new TextDecoder().decode(body));
}
