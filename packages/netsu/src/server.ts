import { randomBytes } from "node:crypto";
import type { Socket as UdpSocket } from "node:dgram";
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
import { JitterTracker } from "./stats.ts";
import type { DataChannel } from "./streams/channel.ts";
import {
  attachReceiver, makeCounters, nextStreamId, startSender, type StreamCounters,
} from "./streams/runner.ts";
import { TcpDataChannel, TcpPipe } from "./transport/tcp.ts";
import {
  Pacer, readUdpHeader, UDP_HEADER_SIZE, udpServerAccept, udpServerBind, writeUdpHeader,
} from "./transport/udp.ts";

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
      const session = new ServerSession(cookie, pipe, this.port);
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
    private port: number,
  ) {}

  wantsStream(cookie: string): boolean {
    // UDP data streams never arrive here — they're picked up by the
    // udpServerBind/udpServerAccept handshake in #acceptUdpStreams, not by
    // net.Server's TCP accept callback (ServerCore.handleConnection). The
    // `#params?.udp !== true` term is a defensive guard against a stray TCP
    // connection carrying the right cookie during a UDP test's
    // CREATE_STREAMS window (e.g. a misbehaving peer): without it such a
    // connection would be silently added as a bogus TCP stream to a UDP
    // session instead of being rejected.
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

      this.#awaitingStreams = true;
      if (params.udp) {
        // The first UDP bind MUST happen before CREATE_STREAMS is announced:
        // real iperf3 clients send their UDP_CONNECT_MSG hello exactly once,
        // with no retry, as soon as they see CREATE_STREAMS — a bind that
        // races the announce can lose that hello and hang the test. See
        // transport/udp.ts's udpServerBind doc and PROTOCOL.md's "UDP
        // specifics". netsu binds lazily per test (unlike iperf3, which
        // binds at startup), so this ordering is load-bearing.
        const first = await udpServerBind(this.port);
        await writeState(pipe, CREATE_STREAMS);
        await this.#acceptUdpStreams(params, first);
      } else {
        await writeState(pipe, CREATE_STREAMS);
        await this.#waitForStreams(params.parallel);
      }
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

  /**
   * The rebind trick (PROTOCOL.md "UDP specifics"): `first` is already bound
   * (before CREATE_STREAMS, by run()). Accept a hello on it, which connect()s
   * it to that stream's peer; then, if more streams remain, bind a fresh
   * SO_REUSEADDR socket on the same port for the next one before accepting
   * again. Streams are opened sequentially by the client (see client.ts's
   * CREATE_STREAMS loop), so binding the next accept socket only after the
   * current stream's hello has been consumed is race-free.
   */
  async #acceptUdpStreams(params: TestParams, first: UdpSocket): Promise<void> {
    let pending = first;
    for (let i = 0; i < params.parallel; i++) {
      const socket = await udpServerAccept(pending, CONTROL_TIMEOUT);
      const id = nextStreamId(this.#streams.length);
      this.#streams.push(this.#makeUdpStream(id, socket, params));
      if (i < params.parallel - 1) pending = await udpServerBind(this.port);
    }
  }

  #makeUdpStream(id: number, socket: UdpSocket, params: TestParams): ServerStream {
    const counters = makeCounters(id);
    let transferError: Error | undefined;
    socket.on("error", (err: Error) => {
      transferError = err;
    });
    let closed = false;
    const close = () => {
      if (closed) return;
      closed = true;
      socket.close();
    };
    if (!params.reverse) {
      const tracker = new JitterTracker();
      socket.on("message", (msg: Buffer) => {
        if (msg.length < UDP_HEADER_SIZE) return;
        const { pcount, sentMs } = readUdpHeader(msg);
        tracker.onPacket(pcount, sentMs, Date.now());
        counters.bytes += msg.length;
      });
      return {
        counters,
        startSending: () => {},
        finalize: () => {
          counters.packets = tracker.received;
          counters.errors = tracker.lost;
          counters.jitter = tracker.jitterMs / 1000; // wire units are seconds
          return transferError;
        },
        close,
      };
    }
    return {
      counters,
      startSending: () => void this.#runUdpSender(socket, counters, params),
      finalize: () => transferError,
      close,
    };
  }

  async #runUdpSender(socket: UdpSocket, counters: StreamCounters, params: TestParams): Promise<void> {
    const buf = randomBytes(params.len);
    const pacer = new Pacer(params.bandwidth);
    let pcount = 0;
    try {
      while (this.#running) {
        await pacer.gate(params.len * 8);
        if (!this.#running) break;
        writeUdpHeader(buf, ++pcount, Date.now());
        socket.send(buf);
        counters.bytes += params.len;
        counters.packets = pcount;
      }
    } catch {
      // closed at test end
    }
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
