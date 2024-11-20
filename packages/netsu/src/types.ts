import * as v from "valibot";
export type Protocol = "tcp" | "udp" | "websocket";
export type TestType = "upload" | "download";

// export interface SpeedTestOptions {
//   duration?: number;
//   chunkSize?: number;
//   port?: number;
//   protocol?: Protocol;
//   testType?: TestType;
//   onProgress?: (speed: number) => void;
// }

export interface SpeedTestResult {
  bytesTransferred: number;
  duration: number;
  speed: number;
  protocol: Protocol;
  testType: TestType;
}

export const TestMessage = v.object({
  type: v.literal("start"),
  testType: v.union([v.literal("upload"), v.literal("download")]),
  chunkSize: v.number(),
});
export type TestMessage = v.InferOutput<typeof TestMessage>;

export type SpeedTestMessage = TestMessage | Uint8Array;

export interface ISpeedTest {
  start(): Promise<void>;
  stop(): void;
  getResult(): SpeedTestResult;
}

export interface SpeedTestClientOptions {
  duration: number;
  chunkSize: number;
  port: number;
  protocol: Protocol;
  testType: TestType;
  onProgress?: (speed: number) => void;
}

export interface SpeedTestServerOptions {
  port: number;
  protocol: Protocol;
  onProgress?: (speed: number) => void;
}
