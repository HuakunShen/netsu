import { Socket, connect } from "node:net";
import { ByteBuffer, type BytePipe } from "../protocol/pipe.ts";
import type { DataChannel } from "../streams/channel.ts";

/** Control-channel view of a TCP socket. */
export class TcpPipe implements BytePipe {
  readonly socket: Socket;
  #buffer = new ByteBuffer();
  #onData = (d: Buffer) => this.#buffer.feed(d);
  #onClose = () => this.#buffer.end();
  #detached = false;

  constructor(socket: Socket) {
    this.socket = socket;
    socket.on("data", this.#onData);
    socket.on("close", this.#onClose);
    socket.on("error", this.#onClose);
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    return this.#buffer.readExact(n, timeoutMs);
  }

  write(data: Uint8Array): Promise<void> {
    if (this.#detached) {
      return Promise.reject(new Error("write on detached TcpPipe"));
    }
    return new Promise((resolve, reject) => {
      this.socket.write(data, (err) => (err ? reject(err) : resolve()));
    });
  }

  /** Stop interpreting bytes; caller takes the raw socket (for TcpDataChannel). */
  detach(): Socket {
    if (this.#buffer.buffered > 0) throw new Error("detach with buffered bytes");
    if (this.#buffer.hasPendingRead) throw new Error("detach with pending readExact");
    this.#detached = true;
    this.socket.off("data", this.#onData);
    this.socket.off("close", this.#onClose);
    this.socket.off("error", this.#onClose);
    return this.socket;
  }

  close(): void {
    // Once detached, the socket belongs to whoever called detach() (typically
    // a TcpDataChannel) — closing here must not reach in and destroy it.
    if (this.#detached) return;
    this.socket.destroy();
    this.#buffer.end();
  }
}

export function tcpConnect(host: string, port: number): Promise<TcpPipe> {
  return new Promise((resolve, reject) => {
    const socket = connect({ host, port, noDelay: true }, () => {
      socket.off("error", reject);
      resolve(new TcpPipe(socket));
    });
    socket.once("error", reject);
  });
}

/** Bulk payload over a detached socket. write() honors kernel backpressure via drain. */
export class TcpDataChannel implements DataChannel {
  #socket: Socket;
  /**
   * A write's callback can fail asynchronously after write() already
   * resolved on the fast path (socket.write() returned true). We cannot
   * reject an already-settled promise, so the error is latched here and
   * surfaced — and the channel poisoned — on the next call.
   */
  #pendingError: Error | undefined;
  #dataListenerAttached = false;

  constructor(socket: Socket) {
    this.#socket = socket;
    socket.on("error", (err) => {
      if (!this.#pendingError) this.#pendingError = err;
      socket.destroy();
    });
  }

  write(chunk: Uint8Array): Promise<void> {
    if (this.#pendingError) return Promise.reject(this.#pendingError);
    return new Promise((resolve, reject) => {
      let settled = false;
      const ok = this.#socket.write(chunk, (err) => {
        if (!err) return;
        if (!this.#pendingError) this.#pendingError = err;
        if (!settled) {
          settled = true;
          reject(err);
        }
        // else: this write already resolved on the fast path; the error is
        // latched in #pendingError and will surface on the next write().
      });
      if (ok) {
        settled = true;
        resolve();
      } else {
        this.#socket.once("drain", () => {
          if (!settled) {
            settled = true;
            resolve();
          }
        });
      }
    });
  }

  /** Single-call contract: registers the one "data" listener for this channel. */
  onData(cb: (byteLength: number) => void): void {
    if (this.#dataListenerAttached) {
      throw new Error("TcpDataChannel.onData may only be called once");
    }
    this.#dataListenerAttached = true;
    this.#socket.on("data", (d: Buffer) => cb(d.length));
  }

  close(): void {
    this.#socket.destroy();
  }
}
