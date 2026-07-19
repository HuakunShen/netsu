import { describe, expect, it } from "vitest";
import { ACCESS_DENIED, COOKIE_SIZE, PARAM_EXCHANGE } from "../src/protocol/states.ts";
import { bytesToCookie, cookieToBytes, makeCookie } from "../src/protocol/cookie.ts";
import { MemoryPipe } from "../src/protocol/pipe.ts";
import { readJson, readState, writeJson, writeState } from "../src/protocol/framing.ts";

describe("cookie", () => {
  it("makes 36-char cookies from the iperf3 alphabet", () => {
    const c = makeCookie();
    expect(c).toHaveLength(36);
    expect(c).toMatch(/^[a-z234567]{36}$/);
    expect(makeCookie()).not.toBe(c);
  });

  it("round-trips through 37-byte NUL-terminated wire form", () => {
    const c = makeCookie();
    const b = cookieToBytes(c);
    expect(b).toHaveLength(COOKIE_SIZE);
    expect(b[36]).toBe(0);
    expect(bytesToCookie(b)).toBe(c);
  });
});

describe("MemoryPipe", () => {
  it("delivers written bytes to the peer, respecting chunk boundaries", async () => {
    const [a, b] = MemoryPipe.pair();
    await a.write(new Uint8Array([1, 2, 3, 4, 5]));
    expect([...(await b.readExact(2))]).toEqual([1, 2]);
    expect([...(await b.readExact(3))]).toEqual([3, 4, 5]);
  });

  it("readExact waits for enough bytes", async () => {
    const [a, b] = MemoryPipe.pair();
    const pending = b.readExact(4);
    await a.write(new Uint8Array([9]));
    await a.write(new Uint8Array([8, 7, 6]));
    expect([...(await pending)]).toEqual([9, 8, 7, 6]);
  });

  it("readExact rejects on close (EOF)", async () => {
    const [a, b] = MemoryPipe.pair();
    const pending = b.readExact(1);
    a.close();
    await expect(pending).rejects.toThrow(/closed/i);
  });
});

describe("framing", () => {
  it("round-trips positive and negative state bytes", async () => {
    const [a, b] = MemoryPipe.pair();
    await writeState(a, PARAM_EXCHANGE);
    await writeState(a, ACCESS_DENIED);
    expect(await readState(b)).toBe(PARAM_EXCHANGE);
    expect(await readState(b)).toBe(ACCESS_DENIED); // signed: 0xff → -1
  });

  it("round-trips JSON with 4-byte BE length prefix", async () => {
    const [a, b] = MemoryPipe.pair();
    const msg = { tcp: true, time: 10, parallel: 2 };
    await writeJson(a, msg);
    expect(await readJson(b)).toEqual(msg);
  });

  it("rejects JSON larger than maxSize", async () => {
    const [a, b] = MemoryPipe.pair();
    await writeJson(a, { pad: "x".repeat(100) });
    await expect(readJson(b, 50)).rejects.toThrow(/too large/i);
  });
});
