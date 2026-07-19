import { Socket, connect } from "node:net";
import { ByteBuffer, type BytePipe } from "../protocol/pipe.ts";
import type { DataChannel } from "../streams/channel.ts";

/** Control-channel view of a TCP socket. */
export class TcpPipe implements BytePipe {
  readonly socket: Socket;
  #buffer = new ByteBuffer();
  #onData = (d: Buffer) => this.#buffer.feed(d);
  #onClose = () => this.#buffer.end();

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
    return new Promise((resolve, reject) => {
      this.socket.write(data, (err) => (err ? reject(err) : resolve()));
    });
  }

  /** Stop interpreting bytes; caller takes the raw socket (for TcpDataChannel). */
  detach(): Socket {
    if (this.#buffer.buffered > 0) throw new Error("detach with buffered bytes");
    this.socket.off("data", this.#onData);
    this.socket.off("close", this.#onClose);
    this.socket.off("error", this.#onClose);
    return this.socket;
  }

  close(): void {
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

  constructor(socket: Socket) {
    this.#socket = socket;
    socket.on("error", () => socket.destroy());
  }

  write(chunk: Uint8Array): Promise<void> {
    return new Promise((resolve, reject) => {
      const ok = this.#socket.write(chunk, (err) => {
        if (err) reject(err);
      });
      if (ok) resolve();
      else this.#socket.once("drain", resolve);
    });
  }

  onData(cb: (byteLength: number) => void): void {
    this.#socket.on("data", (d: Buffer) => cb(d.length));
  }

  close(): void {
    this.#socket.destroy();
  }
}
