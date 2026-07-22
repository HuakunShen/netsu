import { describe, expect, it } from "vitest";
import { startServer } from "../src/server.ts";
import { HAS_IPERF3, nextPort, runIperf3Client } from "./helpers.ts";

interface Iperf3End {
  end: {
    sum_sent: { bytes: number };
    sum_received: { bytes: number };
  };
}

describe.skipIf(!HAS_IPERF3)("official iperf3 client vs netsu server (tcp)", () => {
  it("upload: iperf3 -c completes and reports bytes", async () => {
    const port = await nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client(["-c", "127.0.0.1", "-p", String(port), "-t", "2"]);
      expect(code).toBe(0);
      const r = json as Iperf3End;
      expect(r.end.sum_sent.bytes).toBeGreaterThan(1_000_000);
      expect(r.end.sum_received.bytes).toBeGreaterThan(0);
    } finally {
      await server.close();
    }
  }, 15000);

  it("reverse: iperf3 -c -R receives from netsu server", async () => {
    const port = await nextPort();
    const server = await startServer({ port });
    try {
      const { code, json } = await runIperf3Client(["-c", "127.0.0.1", "-p", String(port), "-t", "2", "-R"]);
      expect(code).toBe(0);
      expect((json as Iperf3End).end.sum_received.bytes).toBeGreaterThan(1_000_000);
    } finally {
      await server.close();
    }
  }, 15000);

  it("parallel: iperf3 -c -P 2", async () => {
    const port = await nextPort();
    const server = await startServer({ port });
    try {
      const { code } = await runIperf3Client(["-c", "127.0.0.1", "-p", String(port), "-t", "2", "-P", "2"]);
      expect(code).toBe(0);
    } finally {
      await server.close();
    }
  }, 15000);
});
