import { createServer, type Socket } from "node:net";
import { bytesToCookie } from "./protocol/cookie.ts";
import { readJson, readState, writeJson, writeState } from "./protocol/framing.ts";
import type { BytePipe } from "./protocol/pipe.ts";
import { decodeParams, type TestParams } from "./protocol/params.ts";
import { decodeResults, encodeResults, type EndResults } from "./protocol/results.ts";
import {
  ACCESS_DENIED, COOKIE_SIZE, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS,
  PARAM_EXCHANGE, SERVER_ERROR, TEST_END, TEST_RUNNING, TEST_START,
} from "./protocol/states.ts";
import type { DataChannel } from "./streams/channel.ts";
import {
  attachReceiver, makeCounters, nextStreamId, startSender, type StreamCounters,
} from "./streams/runner.ts";
import { TcpDataChannel, TcpPipe } from "./transport/tcp.ts";

export interface ServerOptions {
  port?: number;
  transport?: "tcp" | "ws";
}

export interface NetsuServer {
  readonly port: number;
  close(): Promise<void>;
}

const CONTROL_TIMEOUT = 30_000;

export async function startServer(opts: ServerOptions = {}): Promise<NetsuServer> {
  const port = opts.port ?? 5201;
  const transport = opts.transport ?? "tcp";
  if (transport !== "tcp") throw new Error("ws server wired in a later task"); // Task 10 replaces
  const core = new ServerCore(port);

  // Tracks every accepted socket, not just the one bound to core's #active
  // session. A connection can be sitting in the 37-byte cookie readExact (up
  // to 30s) before it is ever attached to a session, so core.abort() alone
  // cannot reach it; close() below destroys whatever is left in this set so
  // it doesn't keep the underlying net.Server (and thus close()'s own
  // callback) pending for the rest of that timeout.
  const sockets = new Set<Socket>();
  const server = createServer({ noDelay: true }, (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    const pipe = new TcpPipe(socket);
    void core.handleConnection(pipe, () => new TcpDataChannel(pipe.detach()));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, () => {
      server.off("error", reject);
      resolve();
    });
  });
  return {
    port,
    close: () =>
      new Promise<void>((resolve) => {
        core.abort();
        for (const socket of sockets) socket.destroy();
        server.close(() => resolve());
      }),
  };
}

/** Accept rule from iperf3's iperf_accept — shared by tcp (Task 8) and ws (Task 10). */
export class ServerCore {
  #active: ServerSession | null = null;

  constructor(readonly port: number) {}

  async handleConnection(pipe: BytePipe, toChannel: () => DataChannel): Promise<void> {
    try {
      const cookie = bytesToCookie(await pipe.readExact(COOKIE_SIZE, CONTROL_TIMEOUT));
      const active = this.#active;
      if (active?.wantsStream(cookie)) {
        active.addStream(toChannel());
        return;
      }
      if (active) {
        await writeState(pipe, ACCESS_DENIED);
        pipe.close();
        return;
      }
      const session = new ServerSession(cookie, pipe);
      this.#active = session;
      try {
        await session.run();
      } finally {
        this.#active = null;
      }
    } catch {
      pipe.close();
    }
  }

  abort(): void {
    this.#active?.abort();
  }
}

interface ServerStream {
  counters: StreamCounters;
  startSending(): void;
  finalize(): Error | undefined;
  close(): void;
}

class ServerSession {
  #streams: ServerStream[] = [];
  #awaitingStreams = false;
  #streamArrived: (() => void) | undefined;
  #waitTimer: ReturnType<typeof setTimeout> | undefined;
  #running = false;
  #startMs = 0;
  #endMs = 0;
  #params: TestParams | undefined;

  constructor(
    readonly cookie: string,
    private pipe: BytePipe,
  ) {}

  wantsStream(cookie: string): boolean {
    // The `#params?.udp !== true` term is dead today: run() throws on
    // params.udp before #awaitingStreams is ever set true, so this method
    // can never observe a UDP session. It is forward-looking for Task 9
    // (UDP data streams use a different accept path), not live protection —
    // do not mistake its current unreachability for a bug, and do not
    // remove it.
    return this.#awaitingStreams && cookie === this.cookie && this.#params?.udp !== true;
  }

  addStream(channel: DataChannel): void {
    const id = nextStreamId(this.#streams.length);
    const counters = makeCounters(id);
    const params = this.#params!;
    if (!params.reverse) attachReceiver(channel, counters);
    // Latched at close() time, before we tear the channel down ourselves —
    // mirrors src/client.ts's #openTcpStream: TcpDataChannel.write() can
    // resolve optimistically on the fast path, so a failure for the very
    // last chunk sent may only be latched on the socket asynchronously, with
    // no further write() call left to surface it on.
    let transferError: Error | undefined;
    let closed = false;
    this.#streams.push({
      counters,
      startSending: () => {
        if (params.reverse) {
          void startSender(channel, counters, params.len, () => this.#running);
        }
      },
      finalize: () => transferError,
      close: () => {
        if (closed) return;
        closed = true;
        transferError = channel.error;
        channel.close();
      },
    });
    this.#streamArrived?.();
  }

  async run(): Promise<void> {
    const pipe = this.pipe;
    try {
      await writeState(pipe, PARAM_EXCHANGE);
      const params = decodeParams(await readJson(pipe, 65536, CONTROL_TIMEOUT));
      this.#params = params;
      if (params.udp) throw new Error("udp wired in a later task"); // Task 9 replaces this line

      this.#awaitingStreams = true;
      await writeState(pipe, CREATE_STREAMS);
      await this.#waitForStreams(params.parallel);
      this.#awaitingStreams = false;

      await writeState(pipe, TEST_START);
      this.#running = true;
      this.#startMs = Date.now();
      await writeState(pipe, TEST_RUNNING);
      for (const s of this.#streams) s.startSending();

      // Safety cap: client owns the timer; +10s grace (see PROTOCOL.md).
      const state = await readState(pipe, params.time * 1000 + 10_000);
      this.#running = false;
      this.#endMs = Date.now();
      if (state !== TEST_END) throw new Error(`expected TEST_END, got ${state}`);

      for (const s of this.#streams) s.close();
      // The server only closes its data streams here, after it has already
      // observed TEST_END on the control channel above — in both forward
      // mode (server is the receiver) and reverse mode (server is the
      // sender, via startSender's `() => this.#running` check, which the
      // TEST_END-driven state change above has already flipped false by the
      // time we get here). So a channel.error latched at close() time
      // reflects a genuine mid-transfer problem in either mode, not
      // teardown-timing noise: the client-side race this mirrors
      // (src/client.ts's duration-timer callback) no longer closes the
      // client's reverse-mode receive streams early either — the client
      // leaves them open until its own #cleanup(), by which point this
      // server has already stopped sending. Still call finalize() on every
      // stream so no latched error is left stranded.
      const finalizeResults = this.#streams.map((s) => s.finalize());
      const failures = finalizeResults.filter((e): e is Error => e !== undefined);
      if (failures.length > 0) {
        throw new Error(`data stream failed: ${failures[0]!.message}`, { cause: failures[0] });
      }
      await writeState(pipe, EXCHANGE_RESULTS);
      decodeResults(await readJson(pipe, 65536, CONTROL_TIMEOUT)); // client's view (kept implicit)
      await writeJson(pipe, encodeResults(this.#localResults()));
      await writeState(pipe, DISPLAY_RESULTS);
      await readState(pipe, CONTROL_TIMEOUT); // IPERF_DONE
    } catch {
      try {
        await writeState(pipe, SERVER_ERROR);
      } catch {
        // control channel already gone
      }
    } finally {
      this.#running = false;
      for (const s of this.#streams) s.close();
      pipe.close();
    }
  }

  #waitForStreams(n: number): Promise<void> {
    return new Promise((resolve, reject) => {
      this.#waitTimer = setTimeout(() => reject(new Error("timed out waiting for data streams")), CONTROL_TIMEOUT);
      const check = () => {
        if (this.#streams.length >= n) {
          clearTimeout(this.#waitTimer);
          this.#waitTimer = undefined;
          this.#streamArrived = undefined;
          resolve();
        }
      };
      this.#streamArrived = check;
      check();
    });
  }

  #localResults(): EndResults {
    const params = this.#params!;
    const sender = params.reverse; // server sends when reversed
    const endSeconds = (this.#endMs - this.#startMs) / 1000;
    return {
      senderHasRetransmits: sender ? 0 : -1,
      streams: this.#streams.map(({ counters }) => ({
        id: counters.id,
        bytes: counters.bytes,
        retransmits: -1,
        jitter: counters.jitter,
        errors: counters.errors,
        packets: counters.packets,
        startTime: 0,
        endTime: endSeconds,
      })),
    };
  }

  abort(): void {
    this.#running = false;
    if (this.#waitTimer) {
      clearTimeout(this.#waitTimer);
      this.#waitTimer = undefined;
    }
    for (const s of this.#streams) s.close();
    this.pipe.close();
  }
}
