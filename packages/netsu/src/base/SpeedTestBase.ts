import type { Protocol, TestType } from "../types";

// This becomes an abstract utility class with shared functionality
export abstract class SpeedTestBase {
  protected bytesTransferred: number = 0;
  protected startTime: number = 0;

  protected calculateSpeed(bytes: number, durationMs: number): number {
    return (bytes * 8) / (1000000 * (durationMs / 1000));
  }

  protected reportProgress(onProgress?: (speed: number) => void): void {
    const currentSpeed = this.calculateSpeed(
      this.bytesTransferred,
      Date.now() - this.startTime
    );
    onProgress?.(currentSpeed);
  }

  protected getResult(protocol: Protocol, testType: TestType) {
    const duration = (Date.now() - this.startTime) / 1000;
    return {
      bytesTransferred: this.bytesTransferred,
      duration,
      speed: this.calculateSpeed(this.bytesTransferred, duration * 1000),
      protocol,
      testType,
    };
  }
}
