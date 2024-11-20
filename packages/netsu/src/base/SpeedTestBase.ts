import type {
  SpeedTestOptions,
  SpeedTestResult,
  Protocol,
  TestType,
} from "../types";

export abstract class SpeedTestBase {
  protected bytesTransferred: number = 0;
  protected startTime: number = 0;
  protected readonly options: Required<SpeedTestOptions>;

  constructor(options: SpeedTestOptions) {
    this.options = {
      duration: 10000,
      chunkSize: 1024 * 1024,
      port: 5201,
      protocol: "tcp",
      testType: "download",
      onProgress: () => {},
      ...options,
    };
  }

  protected calculateSpeed(bytes: number, durationMs: number): number {
    return (bytes * 8) / (1000000 * (durationMs / 1000));
  }

  protected createChunk(): Buffer {
    const chunk = Buffer.alloc(this.options.chunkSize);
    chunk.fill("x");
    return chunk;
  }

  protected reportProgress(): void {
    const currentSpeed = this.calculateSpeed(
      this.bytesTransferred,
      Date.now() - this.startTime,
    );
    this.options.onProgress(currentSpeed);
  }

  protected getResult(): SpeedTestResult {
    const duration = (Date.now() - this.startTime) / 1000;
    return {
      bytesTransferred: this.bytesTransferred,
      duration,
      speed: this.calculateSpeed(this.bytesTransferred, duration * 1000),
      protocol: this.options.protocol,
      testType: this.options.testType,
    };
  }
}
