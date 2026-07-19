import { createServer, type Socket } from "node:net";
import { afterEach, describe, expect, it } from "vitest";
import { TcpDataChannel, TcpPipe, tcpConnect } from "../src/transport/tcp.ts";
import { readJson, writeJson } from "../src/protocol/framing.ts";

const cleanups: (() => void)[] = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

function listen(onConn: (s: Socket) => void): Promise<number> {
  const server = createServer(onConn);
  cleanups.push(() => server.close());
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve((server.address() as { port: number }).port);
    });
  });
}

describe("TcpPipe", () => {
  it("carries framed json both ways", async () => {
    const port = await listen(async (s) => {
      const pipe = new TcpPipe(s);
      const msg = await readJson(pipe);
      await writeJson(pipe, { echo: msg });
    });
    const pipe = await tcpConnect("127.0.0.1", port);
    cleanups.push(() => pipe.close());
    await writeJson(pipe, { hello: 1 });
    expect(await readJson(pipe)).toEqual({ echo: { hello: 1 } });
  });

  it("detach hands over a clean socket for bulk transfer", async () => {
    // Mirrors the protocol's gating: the receiver acks the handshake before
    // the sender starts bulk data, so no payload can coalesce with the
    // handshake bytes and detach() always sees an empty buffer.
    const received: number[] = [];
    let done!: () => void;
    const finished = new Promise<void>((r) => (done = r));
    const port = await listen(async (s) => {
      const pipe = new TcpPipe(s);
      await pipe.readExact(4); // handshake (cookie stand-in)
      await pipe.write(new Uint8Array([1])); // ack — the TEST_START stand-in
      const channel = new TcpDataChannel(pipe.detach());
      channel.onData((n) => {
        received.push(n);
        if (received.reduce((a, b) => a + b, 0) >= 65536) done();
      });
    });
    const pipe = await tcpConnect("127.0.0.1", port);
    cleanups.push(() => pipe.close());
    await pipe.write(new Uint8Array([1, 2, 3, 4]));
    await pipe.readExact(1); // wait for ack before sending bulk
    const channel = new TcpDataChannel(pipe.detach());
    await channel.write(new Uint8Array(65536).fill(7));
    await finished;
    expect(received.reduce((a, b) => a + b, 0)).toBeGreaterThanOrEqual(65536);
  });
});
