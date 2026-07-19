/** Bulk payload channel for data streams (TCP/WS). UDP is packet-based and separate. */
export interface DataChannel {
  /** Backpressure point: resolves when the transport can take more. */
  write(chunk: Uint8Array): Promise<void>;
  onData(cb: (byteLength: number) => void): void;
  close(): void;
}
