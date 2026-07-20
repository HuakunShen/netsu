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
  #detached = false;

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
    if (this.#detached) {
      return Promise.reject(new Error("write on detached WsPipe"));
    }
    return new Promise((resolve, reject) => {
      this.ws.send(data, (err) => (err ? reject(err) : resolve()));
    });
  }

  /** Switch this connection from control framing to bulk payload. */
  detachToChannel(): WsDataChannel {
    if (this.#buffer.buffered > 0) throw new Error("detach with buffered bytes");
    if (this.#buffer.hasPendingRead) throw new Error("detach with pending readExact");
    this.#detached = true;
    this.ws.off("message", this.#onMessage);
    this.ws.off("close", this.#onClose);
    this.ws.off("error", this.#onClose);
    return new WsDataChannel(this.ws);
  }

  close(): void {
    // Once detached, the socket belongs to whoever called detachToChannel()
    // (typically a WsDataChannel) — closing here must not reach in and
    // terminate it.
    if (this.#detached) return;
    this.ws.terminate();
    this.#buffer.end();
  }
}

/**
 * Fix 3: against a peer that completes the TCP handshake but never answers
 * the HTTP Upgrade request (a plain HTTP server, a hung proxy, a stalled TLS
 * terminator — `netsu client --ws` against any of these previously wedged
 * forever with no output, since `ws` has no default handshake timeout of
 * its own).
 *
 * `handshakeTimeout` is passed through to `ws`'s own built-in deadline for
 * the opening handshake, which is honored on real Node.js (confirmed:
 * rejects with "Opening handshake has timed out" right on schedule) — but
 * it is NOT trusted alone: `ws` implements it via the underlying
 * `http.request()`'s `timeout` option, and on Bun 1.3.14 that option's
 * "timeout" event never fires at all, so `ws`'s side of this never rejects
 * and the connection hangs exactly as before, on that runtime. The explicit
 * `setTimeout` below is therefore the actual, runtime-independent deadline;
 * `handshakeTimeout` is kept as a (possibly faster, Node-only) belt-and-
 * suspenders layer on top of it, not the sole mechanism.
 */
const DEFAULT_HANDSHAKE_TIMEOUT_MS = 10_000;

export function wsConnect(
  host: string,
  port: number,
  handshakeTimeoutMs = DEFAULT_HANDSHAKE_TIMEOUT_MS,
): Promise<WsPipe> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const ws = new WebSocket(`ws://${host}:${port}/`, { handshakeTimeout: handshakeTimeoutMs });

    // `onError` is intentionally never detached (only `on`, never `once` +
    // `off`, and no `ws.off("error", onError)` on any settle path): once
    // `settled` flips true it's a permanent no-op, but an EventEmitter with
    // NO "error" listener at all throws on the next "error" instead of
    // emitting quietly — and terminate()ing the socket below (on timeout)
    // can itself surface a further, later "error" from the now-aborted
    // in-flight request. Leaving this listener attached (rather than
    // removing it once we've settled) is what keeps that late, expected
    // error from crashing the process.
    const onError = (err: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(err);
    };
    ws.on("error", onError);

    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      ws.terminate();
      reject(new Error(`ws handshake timeout after ${handshakeTimeoutMs}ms`));
    }, handshakeTimeoutMs);

    ws.once("open", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(new WsPipe(ws));
    });
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
  #dataListenerAttached = false;

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

  /** Single-call contract: registers the one "message" listener for this channel. */
  onData(cb: (byteLength: number) => void): void {
    if (this.#dataListenerAttached) {
      throw new Error("WsDataChannel.onData may only be called once");
    }
    this.#dataListenerAttached = true;
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
