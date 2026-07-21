/** Bulk payload channel for data streams (TCP/WS). UDP is packet-based and separate. */
export interface DataChannel {
  /** Backpressure point: resolves when the transport can take more. */
  write(chunk: Uint8Array): Promise<void>;
  onData(cb: (byteLength: number) => void): void;
  close(): void;
  /**
   * A write failure that arrived asynchronously after its write() call had
   * already resolved on a fast path (so the error could not be delivered via
   * that promise). Callers finalizing a stream's result must consult this
   * after the last write/close to detect a transfer that failed at the very
   * end but otherwise looked clean.
   */
  readonly error: Error | undefined;
}
