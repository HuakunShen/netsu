import { WebSocket, WebSocketServer } from "ws";
import { afterEach, describe, expect, it } from "vitest";
import { WsDataChannel, WsPipe, wsConnect } from "../src/transport/ws.ts";
import { nextPort } from "./helpers.ts";

const cleanups: (() => void)[] = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** Starts a WS server; hands each accepted connection's WsPipe to onPipe. */
function listen(onPipe: (p: WsPipe) => void): Promise<number> {
  const port = nextPort();
  const wss = new WebSocketServer({ port });
  cleanups.push(() => wss.close());
  wss.on("connection", (ws) => onPipe(new WsPipe(ws)));
  return new Promise((resolve, reject) => {
    wss.once("listening", () => resolve(port));
    wss.once("error", reject);
  });
}

function send(ws: WebSocket, data: Uint8Array): Promise<void> {
  return new Promise((resolve, reject) => {
    ws.send(data, (err) => (err ? reject(err) : resolve()));
  });
}

describe("WsPipe reassembly under arbitrary WS-message fragmentation", () => {
  it("reassembles a single protocol unit split across many small WS messages", async () => {
    const original = new Uint8Array(37);
    for (let i = 0; i < 37; i++) original[i] = i + 1;

    let serverPipe!: WsPipe;
    const port = await listen((p) => (serverPipe = p));
    const client = await wsConnect("127.0.0.1", port);
    cleanups.push(() => client.close());

    // readExact is registered BEFORE any fragment arrives, so this exercises
    // the waiter accumulating across several separate "message" events, not
    // just buffering ahead of a read that happens to come later.
    const pending = serverPipe.readExact(37, 2000);

    // Deliberately uneven, unaligned to the 37-byte boundary or to anything
    // meaningful — this is arbitrary fragmentation, not conveniently-sized
    // chunks that happen to equal the unit size.
    const splits = [3, 1, 20, 6, 7];
    expect(splits.reduce((a, b) => a + b, 0)).toBe(37);
    let offset = 0;
    for (const len of splits) {
      await send(client.ws, original.slice(offset, offset + len));
      offset += len;
      await delay(5); // force each fragment through as its own WS message
    }

    const got = await pending;
    expect(Buffer.from(got)).toEqual(Buffer.from(original));
    cleanups.push(() => serverPipe.close());
  });

  it("splits one WS message carrying the tail of one unit plus the head of the next", async () => {
    let serverPipe!: WsPipe;
    const port = await listen((p) => (serverPipe = p));
    const client = await wsConnect("127.0.0.1", port);
    cleanups.push(() => client.close());

    // A single WS message containing 8 bytes: the first 5 are "unit A", the
    // last 3 are the head of "unit B" — the receiver must split this
    // correctly across two separate readExact() calls.
    const payload = new Uint8Array([1, 2, 3, 4, 5, 9, 8, 7]);
    await send(client.ws, payload);
    await delay(20); // let the whole message land in the server pipe's buffer

    const a = await serverPipe.readExact(5, 2000);
    const b = await serverPipe.readExact(3, 2000);
    expect(Buffer.from(a)).toEqual(Buffer.from(payload.slice(0, 5)));
    expect(Buffer.from(b)).toEqual(Buffer.from(payload.slice(5)));
    cleanups.push(() => serverPipe.close());
  });

  it("detachToChannel throws when bytes are still buffered", async () => {
    let serverPipe!: WsPipe;
    const port = await listen((p) => (serverPipe = p));
    const client = await wsConnect("127.0.0.1", port);
    cleanups.push(() => client.close());

    await send(client.ws, new Uint8Array([1, 2, 3]));
    await delay(20); // let the bytes land, unread

    expect(() => serverPipe.detachToChannel()).toThrow(/buffered/);
    cleanups.push(() => serverPipe.close());
  });
});

/**
 * Minimal duck-typed stand-in for `ws`'s WebSocket, exposing only the
 * surface WsDataChannel actually touches (bufferedAmount, readyState, send,
 * on, terminate). Used to make the backpressure gate and error-latching
 * deterministic — real socket-level bufferedAmount growth on loopback is
 * timing-dependent and would make the gate/poll assertions flaky.
 */
class FakeSocket {
  bufferedAmount = 0;
  readyState: number = WebSocket.OPEN;
  #handlers = new Map<string, (err: Error) => void>();
  send = (_data: Uint8Array, cb: (err?: Error) => void): void => cb();
  on = (event: string, cb: (err: Error) => void): this => {
    this.#handlers.set(event, cb);
    return this;
  };
  terminate = (): void => {
    this.readyState = WebSocket.CLOSED;
  };
  emitError(err: Error): void {
    this.#handlers.get("error")?.(err);
  }
}

describe("WsDataChannel backpressure", () => {
  it("does not resolve write() while bufferedAmount stays above the 4MiB high-water mark", async () => {
    const fake = new FakeSocket();
    fake.bufferedAmount = 5 * 1024 * 1024; // over the 4 MiB gate
    const channel = new WsDataChannel(fake as unknown as WebSocket);

    let resolved = false;
    const p = channel.write(new Uint8Array([1])).then(() => {
      resolved = true;
    });
    await delay(30);
    expect(resolved).toBe(false); // still gated — proves the poll loop is real, not a formality

    fake.bufferedAmount = 0; // drains below the high-water mark
    await p;
    expect(resolved).toBe(true);
  });

  it("rejects a gated write() once the socket closes instead of blocking forever", async () => {
    const fake = new FakeSocket();
    fake.bufferedAmount = 5 * 1024 * 1024;
    const channel = new WsDataChannel(fake as unknown as WebSocket);

    const p = channel.write(new Uint8Array([1]));
    await delay(10);
    fake.readyState = WebSocket.CLOSED;
    await expect(p).rejects.toThrow(/closed/);
  });
});

describe("WsDataChannel .error", () => {
  it("latches a send() callback failure and poisons further writes", async () => {
    const fake = new FakeSocket();
    fake.send = (_data, cb) => cb(new Error("simulated send failure"));
    const channel = new WsDataChannel(fake as unknown as WebSocket);

    expect(channel.error).toBeUndefined();
    await expect(channel.write(new Uint8Array([1]))).rejects.toThrow(/simulated send failure/);
    expect(channel.error?.message).toMatch(/simulated send failure/);
    // Stays poisoned — a later write() must not silently "recover".
    await expect(channel.write(new Uint8Array([2]))).rejects.toThrow(/simulated send failure/);
  });

  it("latches an async socket-level error event with no write() in flight", () => {
    const fake = new FakeSocket();
    const channel = new WsDataChannel(fake as unknown as WebSocket);

    expect(channel.error).toBeUndefined();
    fake.emitError(new Error("simulated socket error"));
    expect(channel.error?.message).toBe("simulated socket error");
    expect(fake.readyState).toBe(WebSocket.CLOSED); // the channel terminates the socket
  });
});
