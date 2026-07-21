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

/**
 * Fix 3: without an explicit deadline, connect() to a peer that never
 * completes the TCP handshake (a silently dropped SYN, a firewall that
 * black-holes rather than RSTs) hangs this promise forever with no error at
 * all — mirrors ws.ts's wsConnect fix one layer below the HTTP Upgrade.
 * `socket.setTimeout()` is Node's idiomatic connect-and-idle deadline; it is
 * disabled again as soon as `connect` fires so it doesn't linger as an
 * idle-timeout for the rest of the control channel's life — that's a
 * separate concern, already covered by each readExact() call's own
 * timeoutMs (src/protocol/pipe.ts).
 */
const DEFAULT_CONNECT_TIMEOUT_MS = 10_000;

export function tcpConnect(
  host: string,
  port: number,
  timeoutMs = DEFAULT_CONNECT_TIMEOUT_MS,
): Promise<TcpPipe> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const socket = connect({ host, port, noDelay: true });

    const onTimeout = () => {
      if (settled) return;
      settled = true;
      socket.destroy();
      reject(new Error(`connect timeout after ${timeoutMs}ms`));
    };
    const onError = (err: Error) => {
      if (settled) return;
      settled = true;
      reject(err);
    };
    socket.setTimeout(timeoutMs, onTimeout);
    socket.once("connect", () => {
      if (settled) return;
      settled = true;
      socket.setTimeout(0);
      socket.off("timeout", onTimeout);
      socket.off("error", onError);
      resolve(new TcpPipe(socket));
    });
    socket.once("error", onError);
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

  /**
   * Exposes a write failure latched in #pendingError so it isn't stranded
   * when the last write of a transfer resolves optimistically and its
   * failure (write callback or socket "error") only arrives after that,
   * with no further write() call to surface it on. Callers finalizing a
   * stream (e.g. after the last write, around close()) must check this.
   */
  get error(): Error | undefined {
    return this.#pendingError;
  }

  close(): void {
    // Bare destroy(): any bytes already sitting in the kernel receive queue
    // that Node hasn't yet emitted as a "data" event are discarded, not
    // drained. This is not only a client-side, reverse-mode concern (an
    // earlier version of this comment understated the scope to just that):
    // src/server.ts's ServerSession#run() destroys every stream, including a
    // forward-mode one where the server itself is the receiver, as soon as
    // it observes TEST_END on the control channel — the same no-drain
    // destroy(), on the other side of the same forward-mode transfer.
    //
    // On the client side specifically: the client no longer destroys its
    // reverse-mode receive streams at duration-timer time (src/client.ts's
    // #startRunning leaves them open so the server can observe TEST_END and
    // stop sending first); the only remaining destroy() there happens in
    // #cleanup()'s finally, after the control-channel handshake completes, by
    // which point the server has already stopped writing. So on the client
    // side the truncation this comment used to document in isolation is no
    // longer expected to occur in practice; retained as a note in case a
    // future caller reintroduces an early destroy() on an active receive
    // channel. The server-side forward-mode case above is a separate,
    // still-live instance of the same underlying risk, not a regression to
    // fix here — this comment is only correcting the scope it describes.
    this.#socket.destroy();
  }
}
