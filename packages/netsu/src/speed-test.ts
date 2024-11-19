import { SpeedTestFactory } from "./SpeedTestFactory";
import type { SpeedTestOptions, SpeedTestResult } from "./types";

export * from "./types";
export * from "./SpeedTestFactory";

export function startServer(options: SpeedTestOptions = {}) {
  const server = SpeedTestFactory.createServer(options);
  server.start();
  return server;
}

export function runClient(
  host: string,
  options: SpeedTestOptions = {}
): Promise<SpeedTestResult> {
  const client = SpeedTestFactory.createClient(host, options);
  return client.start();
}
