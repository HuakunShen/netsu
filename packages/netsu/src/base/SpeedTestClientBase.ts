import { SpeedTestBase } from "./SpeedTestBase";
import type { SpeedTestClientOptions, SpeedTestResult } from "../types";

export abstract class SpeedTestClientBase extends SpeedTestBase {
  protected readonly options: SpeedTestClientOptions;

  constructor(options: SpeedTestClientOptions) {
    super();
    this.options = options;
  }

  protected createChunk(): Buffer {
    const chunk = Buffer.alloc(this.options.chunkSize);
    chunk.fill("x");
    return chunk;
  }

  abstract start(): Promise<SpeedTestResult>;
  abstract stop(): void;
}
