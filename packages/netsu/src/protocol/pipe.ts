/** Transport-agnostic ordered byte stream. Control channels always speak this. */
export interface BytePipe {
  /** Resolve with exactly n bytes; reject on EOF/close/timeout. */
  readExact(n: number, timeoutMs?: number): Promise<Uint8Array>;
  /** Resolve when the bytes are handed to the transport (backpressure point). */
  write(data: Uint8Array): Promise<void>;
  close(): void;
}

interface Waiter {
  n: number;
  resolve: (b: Uint8Array) => void;
  reject: (e: Error) => void;
  timer?: ReturnType<typeof setTimeout>;
}

/** Shared buffering logic: transports feed bytes in, readExact pulls them out. */
export class ByteBuffer {
  private chunks: Uint8Array[] = [];
  private length = 0;
  private waiter: Waiter | undefined;
  private closed = false;

  feed(data: Uint8Array): void {
    this.chunks.push(data);
    this.length += data.length;
    this.pump();
  }

  end(): void {
    this.closed = true;
    if (this.waiter) {
      const w = this.waiter;
      this.waiter = undefined;
      if (w.timer) clearTimeout(w.timer);
      w.reject(new Error("pipe closed"));
    }
  }

  get buffered(): number {
    return this.length;
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    if (this.waiter) return Promise.reject(new Error("concurrent readExact"));
    return new Promise((resolve, reject) => {
      const waiter: Waiter = { n, resolve, reject };
      if (timeoutMs !== undefined) {
        waiter.timer = setTimeout(() => {
          this.waiter = undefined;
          reject(new Error(`read timeout after ${timeoutMs}ms`));
        }, timeoutMs);
      }
      this.waiter = waiter;
      if (this.closed && this.length < n) return this.end();
      this.pump();
    });
  }

  private pump(): void {
    const w = this.waiter;
    if (!w || this.length < w.n) return;
    this.waiter = undefined;
    if (w.timer) clearTimeout(w.timer);
    const out = new Uint8Array(w.n);
    let offset = 0;
    while (offset < w.n) {
      const head = this.chunks[0]!;
      const take = Math.min(head.length, w.n - offset);
      out.set(head.subarray(0, take), offset);
      offset += take;
      if (take === head.length) this.chunks.shift();
      else this.chunks[0] = head.subarray(take);
    }
    this.length -= w.n;
    w.resolve(out);
  }
}

/** In-memory pipe pair for unit tests. */
export class MemoryPipe implements BytePipe {
  private buffer = new ByteBuffer();
  private peer!: MemoryPipe;

  static pair(): [MemoryPipe, MemoryPipe] {
    const a = new MemoryPipe();
    const b = new MemoryPipe();
    a.peer = b;
    b.peer = a;
    return [a, b];
  }

  readExact(n: number, timeoutMs?: number): Promise<Uint8Array> {
    return this.buffer.readExact(n, timeoutMs);
  }

  async write(data: Uint8Array): Promise<void> {
    this.peer.buffer.feed(data.slice());
  }

  close(): void {
    this.buffer.end();
    this.peer.buffer.end();
  }
}
