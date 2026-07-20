import { setTimeout as delay } from "node:timers/promises";
import { WebSocket, type RawData } from "ws";
import { ByteBuffer, type BytePipe } from "../protocol/pipe.ts";
import type { DataChannel } from "../streams/channel.ts";

function toBuffer(data: RawData): Buffer {
  if (Buffer.isBuffer(data)) return data;
  if (Array.isArray(data)) return Buffer.concat(data);
  return Buffer.from(data);
}

/** WS binary frames as a byte pipe — identical byte sequence to TCP (PROTOCOL.md). */
export class WsPipe implements BytePipe {
  readonly ws: WebSocket;
  #buffer = new ByteBuffer();
  #onMessage = (d: RawData) => this.#buffer.feed(toBuffer(d));
  #onClose = () => this.#buffer.end();

  constructor(ws: WebSocket) {
    this.ws = ws;
    ws.on("message", this.#onMessage);
    ws.on("close", this.#onClose);
    ws.on("error", this.#onClose);
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    return this.#buffer.readExact(n, timeoutMs);
  }

  write(data: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      this.ws.send(data, (err) => (err ? reject(err) : resolve()));
    });
  }

  /** Switch this connection from control framing to bulk payload. */
  detachToChannel(): WsDataChannel {
    if (this.#buffer.buffered > 0) throw new Error("detach with buffered bytes");
    this.ws.off("message", this.#onMessage);
    this.ws.off("close", this.#onClose);
    this.ws.off("error", this.#onClose);
    return new WsDataChannel(this.ws);
  }

  close(): void {
    this.ws.terminate();
    this.#buffer.end();
  }
}

export function wsConnect(host: string, port: number): Promise<WsPipe> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`ws://${host}:${port}/`);
    ws.once("open", () => {
      ws.off("error", reject);
      resolve(new WsPipe(ws));
    });
    ws.once("error", reject);
  });
}

const HIGH_WATER = 4 * 1024 * 1024;

/** Bulk payload over WS. bufferedAmount polling is the backpressure gate. */
export class WsDataChannel implements DataChannel {
  #ws: WebSocket;
  /**
   * A send() failure — either the callback's error or an async socket-level
   * "error" event with no send() in flight to reject — is latched here so it
   * is never stranded. Mirrors TcpDataChannel's #pendingError: once set, the
   * channel stays poisoned rather than silently recovering, and callers
   * finalizing a stream must consult the `error` getter after the last
   * write/close in case the failure arrived after its write() had already
   * settled.
   */
  #pendingError: Error | undefined;

  constructor(ws: WebSocket) {
    this.#ws = ws;
    ws.on("error", (err: Error) => {
      if (!this.#pendingError) this.#pendingError = err;
      ws.terminate();
    });
  }

  async write(chunk: Uint8Array): Promise<void> {
    if (this.#pendingError) throw this.#pendingError;
    while (this.#ws.bufferedAmount > HIGH_WATER) {
      if (this.#ws.readyState !== WebSocket.OPEN) throw new Error("ws closed");
      await delay(2);
    }
    if (this.#ws.readyState !== WebSocket.OPEN) throw new Error("ws closed");
    return new Promise((resolve, reject) => {
      this.#ws.send(chunk, (err) => {
        if (err) {
          if (!this.#pendingError) this.#pendingError = err;
          reject(err);
        } else {
          resolve();
        }
      });
    });
  }

  onData(cb: (byteLength: number) => void): void {
    this.#ws.on("message", (d: RawData) => cb(toBuffer(d).length));
  }

  /**
   * Exposes a send failure latched in #pendingError so it isn't stranded
   * when the last write of a transfer resolves and its failure only arrives
   * afterward (or arrives as a bare socket "error" with no write() call left
   * to surface it on). Callers finalizing a stream must check this — see
   * DataChannel's doc comment.
   */
  get error(): Error | undefined {
    return this.#pendingError;
  }

  close(): void {
    this.#ws.terminate();
  }
}
