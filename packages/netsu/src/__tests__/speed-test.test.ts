import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { startServer, runClient, type SpeedTestResult } from "../speed-test";

describe("Speed Test", () => {
  const TEST_PORT = 5202; // Using different port than default to avoid conflicts
  let server: { stop: () => void };

  beforeAll(() => {
    server = startServer({
      port: TEST_PORT,
      duration: 2000, // Shorter duration for tests
    });
  });

  afterAll(() => {
    server.stop();
  });

  it("should complete a speed test between client and server", async () => {
    const result = await runClient("localhost", {
      port: TEST_PORT,
      duration: 2000,
    });
    console.log(result);

    // Verify the structure and basic validity of results
    expect(result).toHaveProperty("bytesTransferred");
    expect(result).toHaveProperty("duration");
    expect(result).toHaveProperty("speed");

    expect(result.bytesTransferred).toBeGreaterThan(0);
    expect(result.duration).toBeGreaterThan(0);
    expect(result.speed).toBeGreaterThan(0);

    // The speed should be reasonable for a local connection
    // (typically at least a few hundred Mbps)
    expect(result.speed).toBeGreaterThan(100);
  }, 5000); // Increasing timeout to 5 seconds

  it("should report progress during the test", async () => {
    const progressUpdates: number[] = [];

    const result = await runClient("localhost", {
      port: TEST_PORT,
      duration: 2000,
      onProgress: (speed) => {
        progressUpdates.push(speed);
      },
    });

    // Should have received multiple progress updates
    expect(progressUpdates.length).toBeGreaterThan(0);

    // Progress speeds should be positive numbers
    progressUpdates.forEach((speed) => {
      expect(speed).toBeGreaterThan(0);
    });
  }, 5000);
});
