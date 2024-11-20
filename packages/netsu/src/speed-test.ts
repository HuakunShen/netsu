import { SpeedTestFactory } from "./SpeedTestFactory";
import type { SpeedTestOptions, SpeedTestResult } from "./types";

export * from "./types";
export * from "./SpeedTestFactory";

export function startServer(options: SpeedTestOptions = {}) {
  options = Object.assign({ protocol: "tcp" }, options);
  const server = SpeedTestFactory.createServer(options);
  server.start();
  return server;
}

export function runClient(
  host: string,
  options: SpeedTestOptions = {}
): Promise<SpeedTestResult> {
  options = Object.assign({ protocol: "tcp" }, options);
  const client = SpeedTestFactory.createClient(host, options);
  return client.start();
}
