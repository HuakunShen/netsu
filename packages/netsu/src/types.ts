export type Protocol = "tcp" | "udp" | "websocket";
export type TestType = "upload" | "download";

export interface SpeedTestOptions {
  duration?: number;
  chunkSize?: number;
  port?: number;
  protocol?: Protocol;
  testType?: TestType;
  onProgress?: (speed: number) => void;
}

export interface SpeedTestResult {
  bytesTransferred: number;
  duration: number;
  speed: number;
  protocol: Protocol;
  testType: TestType;
}

export interface TestMessage {
  type: "start";
  testType: TestType;
}

export type SpeedTestMessage = TestMessage | Uint8Array;

export interface ISpeedTest {
  start(): Promise<void>;
  stop(): void;
  getResult(): SpeedTestResult;
}
