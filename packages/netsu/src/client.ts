import type { Socket as UdpSocket } from "node:dgram";
import { cookieToBytes, makeCookie } from "./protocol/cookie.ts";
import { readJson, readState, writeJson, writeState } from "./protocol/framing.ts";
import type { BytePipe } from "./protocol/pipe.ts";
import {
  DEFAULT_TCP_LEN, DEFAULT_UDP_BANDWIDTH, DEFAULT_UDP_LEN,
  encodeParams, type TestParams,
} from "./protocol/params.ts";
import { decodeResults, encodeResults, type EndResults } from "./protocol/results.ts";
import {
  ACCESS_DENIED, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS, IPERF_DONE,
  IPERF_START, PARAM_EXCHANGE, SERVER_ERROR, TEST_END, TEST_RUNNING, TEST_START,
} from "./protocol/states.ts";
import { bitsPerSecond, IntervalMeter, type IntervalReport, JitterTracker } from "./stats.ts";
import {
  attachReceiver, makeCounters, nextStreamId, startSender, type StreamCounters,
} from "./streams/runner.ts";
import { TcpDataChannel, tcpConnect } from "./transport/tcp.ts";
import {
  Pacer, probeMaxUdpSendLen, readUdpHeader, SendBufferPool, tryRaiseUdpSendBuffer,
  UDP_HEADER_SIZE, UDP_SEND_UNAVAILABLE, udpClientConnect, writeUdpHeader,
} from "./transport/udp.ts";
import { wsConnect } from "./transport/ws.ts";

export interface ClientOptions {
  port?: number;
  transport?: "tcp" | "ws";
  udp?: boolean;
  reverse?: boolean;
  duration?: number; // seconds, default 10
  parallel?: number; // default 1
  len?: number; // blksize
  bandwidth?: number; // bits/s, UDP pacing
  interval?: number; // seconds between onInterval calls; 0 disables
  onInterval?: (report: IntervalReport) => void;
}

export interface UdpStats {
  jitterMs: number;
  lost: number;
  packets: number;
  lostPercent: number;
}

export interface TestResult {
  udp: boolean;
  reverse: boolean;
  durationSeconds: number;
  sentBytes: number;
  receivedBytes: number;
  sendBitsPerSecond: number;
  receiveBitsPerSecond: number;
  local: EndResults;
  remote: EndResults;
  udpStats?: UdpStats;
}

const CONTROL_TIMEOUT = 30_000;

interface StreamHandle {
  counters: StreamCounters;
  start(): void;
  /**
   * Copy async trackers (e.g. UDP jitter) into counters before results.
   * Returns a transport error latched on the channel (e.g. a write failure
   * that arrived asynchronously after the last write() already resolved on
   * its fast path) so the caller does not report a failed transfer as clean.
   */
  finalize(): Error | undefined;
  close(): void;
}

export async function runClient(host: string, opts: ClientOptions = {}): Promise<TestResult> {
  const udp = opts.udp ?? false;
  const params: TestParams = {
    udp,
    time: opts.duration ?? 10,
    parallel: opts.parallel ?? 1,
    len: opts.len ?? (udp ? DEFAULT_UDP_LEN : DEFAULT_TCP_LEN),
    reverse: opts.reverse ?? false,
    bandwidth: opts.bandwidth ?? (udp ? DEFAULT_UDP_BANDWIDTH : 0),
  };
  const session = new ClientSession(host, opts.port ?? 5201, opts.transport ?? "tcp", params, opts);
  return session.run();
}

class ClientSession {
  readonly cookie = makeCookie();
  #streams: StreamHandle[] = [];
  #meter = new IntervalMeter(Date.now());
  #running = false;
  #startMs = 0;
  #endMs = 0;
  #remote: EndResults | undefined;
  #endTimer: ReturnType<typeof setTimeout> | undefined;
  #intervalTimer: ReturnType<typeof setInterval> | undefined;

  constructor(
    private host: string,
    private port: number,
    private transport: "tcp" | "ws",
    private params: TestParams,
    private opts: ClientOptions,
  ) {}

  async run(): Promise<TestResult> {
    const control = await this.#connectControl();
    try {
      await control.write(cookieToBytes(this.cookie));
      for (;;) {
        const timeout = this.#running
          ? this.params.time * 1000 + CONTROL_TIMEOUT
          : CONTROL_TIMEOUT;
        const state = await readState(control, timeout);
        switch (state) {
          case IPERF_START:
            break; // informational, ignore
          case PARAM_EXCHANGE:
            await writeJson(control, encodeParams(this.params));
            // Fix 3: when this client will be the UDP sender (forward mode),
            // refuse here — before CREATE_STREAMS opens any stream — if this
            // host/runtime cannot emit even a bare UDP_HEADER_SIZE datagram,
            // rather than silently proceeding at an untested size only to
            // have every send fail once streaming starts.
            if (this.params.udp && !this.params.reverse) {
              await this.#assertUdpSendable();
            }
            break;
          case CREATE_STREAMS:
            for (let i = 0; i < this.params.parallel; i++) {
              this.#streams.push(await this.#openStream(nextStreamId(this.#streams.length)));
            }
            break;
          case TEST_START:
            break; // streams already open; wait for TEST_RUNNING
          case TEST_RUNNING:
            this.#startRunning(control);
            break;
          case EXCHANGE_RESULTS: {
            // This can arrive before our own end timer fires: this netsu
            // server (src/server.ts) reads TEST_END with a `time + 10s`
            // safety-cap timeout (PROTOCOL.md only requires that the server
            // "caps the test"; it does not drive EXCHANGE_RESULTS itself —
            // if that cap expires it throws and sends SERVER_ERROR instead).
            // A non-netsu peer could still legitimately drive
            // EXCHANGE_RESULTS early on its own schedule, so we handle it
            // defensively either way. Without this, #endMs stays 0 and
            // endSeconds below becomes a large negative number sent on the
            // wire as end_time. Also disarm our own end timer and clear
            // #running here: if we didn't, the timer would still fire later
            // and (a) write a stray TEST_END byte onto the control channel
            // while we're already in DISPLAY_RESULTS, and (b) overwrite
            // #endMs after it's already gone out on the wire in
            // encodeResults() below, desyncing durationSeconds from the
            // reported end_time. This makes the end-of-test path idempotent
            // regardless of which side drives it first.
            if (this.#endMs === 0) {
              this.#endMs = Date.now();
              this.#running = false;
              if (this.#endTimer) {
                clearTimeout(this.#endTimer);
                this.#endTimer = undefined;
              }
            }
            const failures = this.#streams
              .map((s) => s.finalize())
              .filter((e): e is Error => e !== undefined);
            if (failures.length > 0) {
              if (this.params.udp) {
                // Fix 1(b): real iperf3 counts UDP send errors (e.g. a
                // transient ENOBUFS under load, or a `len` this host's
                // socket can't emit — see Fix 1(a)) and continues the test
                // rather than aborting it. Log for diagnosability and keep
                // going; only a TCP write failure below is a genuine
                // transfer failure.
                for (const f of failures) {
                  console.error(`netsu: udp stream error (continuing): ${f.message}`);
                }
              } else {
                throw new Error(`data stream failed: ${failures[0]!.message}`, {
                  cause: failures[0],
                });
              }
            }
            await writeJson(control, encodeResults(this.#localResults()));
            this.#remote = decodeResults(await readJson(control, 65536, CONTROL_TIMEOUT));
            break;
          }
          case DISPLAY_RESULTS:
            await writeState(control, IPERF_DONE);
            return this.#buildResult();
          case ACCESS_DENIED:
            throw new Error("server busy (ACCESS_DENIED)");
          case SERVER_ERROR:
            throw new Error("server reported error (SERVER_ERROR)");
          default:
            throw new Error(`unexpected control state ${state}`);
        }
      }
    } finally {
      this.#cleanup(control);
    }
  }

  #connectControl(): Promise<BytePipe> {
    return this.transport === "ws"
      ? wsConnect(this.host, this.port)
      : tcpConnect(this.host, this.port);
  }

  async #openStream(id: number): Promise<StreamHandle> {
    if (this.params.udp) return this.#openUdpStream(id);
    if (this.transport === "ws") return this.#openWsStream(id);
    return this.#openTcpStream(id);
  }

  async #openWsStream(id: number): Promise<StreamHandle> {
    const pipe = await wsConnect(this.host, this.port);
    await pipe.write(cookieToBytes(this.cookie));
    const channel = pipe.detachToChannel();
    const counters = makeCounters(id);
    if (this.params.reverse) {
      attachReceiver(channel, counters, (n) => this.#meter.add(n));
    }
    // Latched at close() time, before we tear the channel down ourselves.
    // Unlike TcpDataChannel, WsDataChannel.write() has no fast path — it
    // resolves only inside the send() callback — so this isn't about a
    // fast-path race. What makes `error` load-bearing here is the bare
    // socket "error" event (see ws.ts's WsDataChannel doc comment): it can
    // arrive with no write() in flight to reject, so a mid-transfer failure
    // may only ever be latched asynchronously, with no further write() call
    // left to surface it on.
    let transferError: Error | undefined;
    let closed = false;
    return {
      counters,
      start: () => {
        if (!this.params.reverse) {
          void startSender(channel, counters, this.params.len, () => this.#running, (n) =>
            this.#meter.add(n),
          );
        }
      },
      finalize: () => transferError,
      close: () => {
        if (closed) return;
        closed = true;
        transferError = channel.error;
        channel.close();
      },
    };
  }

  async #openTcpStream(id: number): Promise<StreamHandle> {
    const pipe = await tcpConnect(this.host, this.port);
    await pipe.write(cookieToBytes(this.cookie));
    const channel = new TcpDataChannel(pipe.detach());
    const counters = makeCounters(id);
    if (this.params.reverse) {
      attachReceiver(channel, counters, (n) => this.#meter.add(n));
    }
    // Latched at close() time, before we tear the channel down ourselves —
    // see the `close` doc below for why.
    let transferError: Error | undefined;
    let closed = false;
    return {
      counters,
      start: () => {
        if (!this.params.reverse) {
          void startSender(channel, counters, this.params.len, () => this.#running, (n) =>
            this.#meter.add(n),
          );
        }
      },
      finalize: () => transferError,
      close: () => {
        if (closed) return;
        closed = true;
        // Read channel.error BEFORE destroying the socket: TcpDataChannel's
        // write() can resolve optimistically on the fast path, so a failure
        // for the very last chunk we sent may only be latched on the socket
        // asynchronously, with no further write() call left to surface it
        // on — checking here catches that. Once we call channel.close()
        // (a bare socket.destroy()), the remote side closing at essentially
        // the same moment can itself produce a late, self-inflicted async
        // error on this same channel; capturing the pre-close snapshot
        // keeps that teardown noise from being misreported as a genuine
        // transfer failure.
        transferError = channel.error;
        channel.close();
      },
    };
  }

  /**
   * UDP data stream: connect handshake (transport/udp.ts), then either
   * receive+track (reverse) or pace+send (forward). `finalize()` returns any
   * error latched by the socket's persistent "error" listener (e.g. a
   * connect()ed socket's send hitting an ICMP port-unreachable after the
   * peer went away) — mirrors the `channel.error` pattern #openTcpStream
   * uses, so a mid-transfer UDP failure isn't reported as a clean run.
   */
  async #openUdpStream(id: number): Promise<StreamHandle> {
    const socket = await udpClientConnect(this.host, this.port);
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
    if (this.params.reverse) {
      const tracker = new JitterTracker();
      socket.on("message", (msg: Buffer) => {
        if (msg.length < UDP_HEADER_SIZE) return;
        const { pcount, sentMs } = readUdpHeader(msg);
        tracker.onPacket(pcount, sentMs, Date.now());
        counters.bytes += msg.length;
        this.#meter.add(msg.length);
      });
      return {
        counters,
        start: () => {},
        finalize: () => {
          // iperf3's receiver-reported packet_count is the max pcount seen
          // (received + lost), not just what arrived — see JitterTracker.
          counters.packets = tracker.maxSeq;
          counters.errors = tracker.lost;
          counters.jitter = tracker.jitterMs / 1000; // wire units are seconds
          return transferError;
        },
        close,
      };
    }
    return {
      counters,
      start: () => void this.#runUdpSender(socket, counters, (err) => {
        transferError = err;
      }),
      finalize: () => transferError,
      close,
    };
  }

  /**
   * Fix 3: run a standalone probe at PARAM_EXCHANGE time (before any UDP
   * stream is opened) and throw if this host/runtime cannot send anything
   * at all. Thrown here, this propagates straight out of run() with no
   * catch in between — a clear diagnostic instead of opening streams that
   * would silently transfer zero bytes at an untested "worked" size.
   */
  async #assertUdpSendable(): Promise<void> {
    const len = await probeMaxUdpSendLen(this.params.len);
    if (len === UDP_SEND_UNAVAILABLE) {
      throw new Error(
        `netsu: cannot send any UDP datagram on this host/runtime (probed down to ${UDP_HEADER_SIZE} bytes and failed) — refusing to start a UDP test that could never transfer data`,
      );
    }
  }

  async #runUdpSender(
    socket: UdpSocket,
    counters: StreamCounters,
    onError: (err: Error) => void,
  ): Promise<void> {
    const requested = this.params.len;
    // Fix 1(a): the negotiated `len` (from opts.len or the UDP default) is
    // not always a size this host/runtime can actually put on the wire —
    // see transport/udp.ts's probeMaxUdpSendLen doc. This is unilateral and
    // never renegotiated with the peer: the receiver never validates an
    // arriving datagram's size against `len`, so shrinking only our own
    // send chunk size is wire-compatible.
    tryRaiseUdpSendBuffer(socket, requested * 2);
    const len = await probeMaxUdpSendLen(requested);
    if (len === UDP_SEND_UNAVAILABLE) {
      // Fix 3: #assertUdpSendable already gates this at PARAM_EXCHANGE for
      // the common case — this is a defensive fallback in case conditions
      // changed between that check and stream start (e.g. this per-stream
      // probe socket landing on a different code path), so a genuinely
      // unsendable stream is still a counted error, never a silent no-op
      // sender loop.
      onError(new Error(`netsu: cannot send any UDP datagram on this host/runtime`));
      return;
    }
    if (len < requested) {
      console.error(
        `netsu: udp len ${requested} exceeds the largest datagram this host can send (${len} bytes); sending ${len}-byte datagrams instead`,
      );
    }
    const pool = new SendBufferPool(len);
    const pacer = new Pacer(this.params.bandwidth);
    let pcount = 0;
    try {
      while (this.#running) {
        await pacer.gate(len * 8);
        if (!this.#running) break;
        const buf = pool.acquire();
        writeUdpHeader(buf, ++pcount, performance.timeOrigin + performance.now());
        counters.packets = pcount;
        socket.send(buf, (err) => {
          pool.release(buf);
          // Fix 1: only a datagram that actually left the process (no err)
          // counts toward bytes sent — crediting every attempt (including
          // ones EMSGSIZE/ENOBUFS'd away, see Fix 1(a)/(b) above) reported a
          // full byte count on a run that transferred nothing. `pcount`
          // above still advances per attempt regardless, since the wire
          // header's sequence number — and the receiver's loss accounting
          // built on it — must not gap-fill around a locally-known failure.
          if (err) {
            counters.errors++;
            onError(err);
          } else {
            counters.bytes += len;
            this.#meter.add(len);
          }
        });
      }
    } catch {
      // socket closed at test end
    }
  }

  #startRunning(control: BytePipe): void {
    this.#running = true;
    this.#startMs = Date.now();
    this.#meter = new IntervalMeter(this.#startMs);
    for (const s of this.#streams) s.start();

    const intervalSec = this.opts.interval ?? 1;
    if (intervalSec > 0 && this.opts.onInterval) {
      this.#intervalTimer = setInterval(() => {
        this.opts.onInterval?.(this.#meter.snap(Date.now()));
      }, intervalSec * 1000);
    }

    this.#endTimer = setTimeout(() => {
      this.#running = false;
      this.#endMs = Date.now();
      if (this.#intervalTimer) clearInterval(this.#intervalTimer);
      // Real iperf3 signals end-of-test on the control channel FIRST, then
      // tears down data fds — send TEST_END before closing streams so we
      // don't invert that order. In reverse mode, the client is the
      // *receiver*: the server is the one still sending, driven by its
      // startSender loop that only stops once it observes TEST_END on the
      // control channel. If we closed our receive streams here too, we'd be
      // racing an un-awaited, same-tick TEST_END write against the server's
      // startSender — the server hasn't processed TEST_END yet, so it is
      // still writing into a socket we just RST'd via destroy(), which
      // latches a spurious EPIPE/ECONNRESET on its side. So in reverse mode
      // we leave the streams open here; #cleanup()'s finally already closes
      // every stream once the control-channel handshake (EXCHANGE_RESULTS
      // onward) completes, by which point the server has stopped sending on
      // its own. In forward mode the client is the sender and owns the
      // stream lifecycle, so closing here (before the server has finished
      // reading) is correct and unchanged.
      void writeState(control, TEST_END).catch(() => {});
      if (!this.params.reverse) {
        for (const s of this.#streams) s.close();
      }
    }, this.params.time * 1000);
  }

  #localResults(): EndResults {
    const sender = !this.params.reverse;
    const endSeconds = (this.#endMs - this.#startMs) / 1000;
    return {
      senderHasRetransmits: sender ? 0 : -1,
      streams: this.#streams.map(({ counters }) => ({
        id: counters.id,
        bytes: counters.bytes,
        retransmits: -1, // no TCP_INFO from pure Node — see PROTOCOL.md
        jitter: counters.jitter,
        errors: counters.errors,
        packets: counters.packets,
        startTime: 0,
        endTime: endSeconds,
      })),
    };
  }

  #buildResult(): TestResult {
    const local = this.#localResults();
    const remote = this.#remote;
    if (!remote) throw new Error("no results from server");
    const duration = (this.#endMs - this.#startMs) / 1000;
    const sum = (r: EndResults) => r.streams.reduce((a, s) => a + s.bytes, 0);
    const sender = !this.params.reverse;
    const sentBytes = sender ? sum(local) : sum(remote);
    const receivedBytes = sender ? sum(remote) : sum(local);
    const result: TestResult = {
      udp: this.params.udp,
      reverse: this.params.reverse,
      durationSeconds: duration,
      sentBytes,
      receivedBytes,
      sendBitsPerSecond: bitsPerSecond(sentBytes, duration),
      receiveBitsPerSecond: bitsPerSecond(receivedBytes, duration),
      local,
      remote,
    };
    if (this.params.udp) {
      const receiverSide = sender ? remote : local;
      // `packets` is the receiver-reported packet_count, which (per
      // JitterTracker.maxSeq / Fix 8) already equals received + lost —
      // iperf3's own convention. lost_percent is therefore lost/packets,
      // NOT lost/(packets+lost): that older formula assumed `packets` was
      // just the received count and would double-count the lost packets.
      const packets = receiverSide.streams.reduce((a, s) => a + s.packets, 0);
      const lost = receiverSide.streams.reduce((a, s) => a + s.errors, 0);
      const jitterMs =
        (receiverSide.streams.reduce((a, s) => a + s.jitter, 0) /
          Math.max(1, receiverSide.streams.length)) * 1000;
      result.udpStats = {
        jitterMs, lost, packets,
        lostPercent: packets > 0 ? (100 * lost) / packets : 0,
      };
    }
    return result;
  }

  #cleanup(control: BytePipe): void {
    this.#running = false;
    if (this.#endTimer) clearTimeout(this.#endTimer);
    if (this.#intervalTimer) clearInterval(this.#intervalTimer);
    for (const s of this.#streams) s.close();
    control.close();
  }
}
