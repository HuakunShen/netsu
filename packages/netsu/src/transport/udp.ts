import { createSocket, type RemoteInfo, type Socket } from "node:dgram";
import { setTimeout as delay } from "node:timers/promises";

// iperf3 stream-setup magic values (iperf_udp.c: UDP_CONNECT_MSG / UDP_CONNECT_REPLY).
//
// PROTOCOL.md/the task brief give these as UDP_CONNECT_MSG=0x36373839,
// UDP_CONNECT_REPLY=0x39383736, LEGACY=987654321 (decimal), intending them to
// be read/written big-endian like the packet header below. That does NOT
// match real iperf3 on the wire, and is a genuine PROTOCOL.md/brief error —
// flagged per the task instructions rather than silently "fixed" over.
//
// esnet/iperf's iperf.h defines these values per-CPU-endianness specifically
// so that a raw, un-swapped `write(s, &buf, sizeof(buf))` of a host-native
// `unsigned int` produces the SAME wire bytes regardless of host endianness
// (verified directly against master's iperf_udp.c/iperf.h):
//   #if BYTE_ORDER == BIG_ENDIAN
//     UDP_CONNECT_MSG = 0x39383736; UDP_CONNECT_REPLY = 0x36373839; ...
//   #else  // little-endian (x86/ARM — effectively every real host)
//     UDP_CONNECT_MSG = 0x36373839; UDP_CONNECT_REPLY = 0x39383736; ...
//   #endif
// On a little-endian host, `write()`ing the little-endian *in-memory* layout
// of 0x36373839 puts bytes [0x39,0x38,0x37,0x36] on the wire — i.e. reading
// that wire value as a big-endian u32 (as this file's readUdpHeader-style
// helpers do) yields 0x39383736, not 0x36373839. Confirmed empirically
// against real iperf3 3.21 on this machine: its UDP_CONNECT_REPLY arrives on
// the wire as bytes `36 37 38 39`, which is 0x36373839 read big-endian — the
// mirror image of what the brief specified. The values below are that
// corrected, wire-accurate (big-endian-read) form.
//
// Also note: iperf_udp_accept() (server) does NOT validate the hello's
// content at all — it recvfrom()s the first datagram purely to learn the
// peer's address and unconditionally replies. netsu's server still checks
// for UDP_CONNECT_MSG as a light sanity filter against stray traffic; this
// is stricter than real iperf3 but compatible with it, since a genuine
// iperf3 client always sends exactly this value.
export const UDP_CONNECT_MSG = 0x39383736;
export const UDP_CONNECT_REPLY = 0x36373839;
export const LEGACY_UDP_CONNECT_REPLY = 0xb168de3a;

/** sec(u32BE) | usec(u32BE) | pcount(u32BE), rest of the datagram is filler. */
export const UDP_HEADER_SIZE = 12;

export function writeUdpHeader(buf: Buffer, pcount: number, nowMs: number): void {
  const sec = Math.floor(nowMs / 1000);
  const usec = Math.floor((nowMs % 1000) * 1000);
  buf.writeUInt32BE(sec >>> 0, 0);
  buf.writeUInt32BE(usec >>> 0, 4);
  buf.writeUInt32BE(pcount >>> 0, 8);
}

export function readUdpHeader(buf: Buffer): { pcount: number; sentMs: number } {
  const sec = buf.readUInt32BE(0);
  const usec = buf.readUInt32BE(4);
  const pcount = buf.readUInt32BE(8);
  return { pcount, sentMs: sec * 1000 + usec / 1000 };
}

/**
 * Token-bucket pacing: gate(bits) accounts `bits` against the configured
 * rate and resolves once (cumulative bits sent) / rate has actually
 * elapsed since construction, so a tight loop of calls is smoothed to the
 * target bitrate rather than firing as fast as the event loop allows.
 * rate <= 0 disables pacing (never the case for netsu's UDP defaults —
 * see DEFAULT_UDP_BANDWIDTH — but kept for completeness/testability).
 */
export class Pacer {
  #rate: number;
  #startMs = Date.now();
  #bitsSent = 0;

  constructor(bitsPerSecond: number) {
    this.#rate = bitsPerSecond;
  }

  async gate(bits: number): Promise<void> {
    this.#bitsSent += bits;
    if (this.#rate <= 0) return;
    const idealMs = (this.#bitsSent / this.#rate) * 1000;
    const aheadMs = idealMs - (Date.now() - this.#startMs);
    if (aheadMs > 1) await delay(aheadMs);
  }
}

/**
 * Client side of iperf3's UDP stream setup (iperf_udp_connect): send
 * UDP_CONNECT_MSG from a fresh (optionally-connected) socket, wait for
 * UDP_CONNECT_REPLY (or the legacy reply value), then connect() so the
 * kernel pins the 4-tuple for the rest of the stream. 5s timeout, no retry
 * (matches real iperf3 — a lost hello just times out).
 */
export function udpClientConnect(host: string, port: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = createSocket("udp4");
    let settled = false;

    const onError = (err: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        socket.close();
      } catch {
        // already closed
      }
      reject(err);
    };
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      socket.off("error", onError);
      socket.close();
      reject(new Error("udp connect timeout"));
    }, 5000);
    socket.on("error", onError);

    socket.connect(port, host, () => {
      const onMessage = (msg: Buffer) => {
        const v = msg.length >= 4 ? msg.readUInt32BE(0) : -1;
        if (v !== UDP_CONNECT_REPLY && v !== LEGACY_UDP_CONNECT_REPLY) return;
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        socket.off("error", onError);
        socket.off("message", onMessage);
        resolve(socket);
      };
      socket.on("message", onMessage);
      const hello = Buffer.alloc(4);
      hello.writeUInt32BE(UDP_CONNECT_MSG, 0);
      socket.send(hello);
    });
  });
}

/**
 * Bind a stream-accept socket on the shared UDP port. reuseAddr lets a
 * fresh socket bind while earlier (now connect()ed) stream sockets keep
 * the port — the kernel routes each pinned 4-tuple to its own connected
 * socket, and anything unmatched (a fresh hello) lands on this listener.
 *
 * The FIRST bind of a test must complete before CREATE_STREAMS is
 * announced on the control channel: official iperf3 clients send their
 * UDP_CONNECT_MSG hello exactly once, with no retry, immediately on
 * seeing CREATE_STREAMS — a bind that happens after the announce can lose
 * the race and silently drop the hello, hanging the test. See PROTOCOL.md
 * "UDP specifics".
 */
export function udpServerBind(port: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = createSocket({ type: "udp4", reuseAddr: true });
    const onError = (err: Error) => {
      // Best-effort: bind failures may already have torn the handle down,
      // and dgram throws on a redundant close() — never let cleanup mask
      // the real error.
      try {
        socket.close();
      } catch {
        // already closed
      }
      reject(err);
    };
    socket.once("error", onError);
    socket.bind(port, () => {
      socket.off("error", onError);
      resolve(socket);
    });
  });
}

/**
 * Server side of iperf3's UDP stream setup (iperf_udp_accept): wait for
 * UDP_CONNECT_MSG on a bound socket, connect() to the sender (pinning the
 * 4-tuple), reply UDP_CONNECT_REPLY, and resolve with the same (now
 * connected) socket. Callers loop bind -> accept for each parallel
 * stream — the rebind trick described above.
 */
export function udpServerAccept(socket: Socket, timeoutMs: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    let settled = false;

    const onError = (err: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.off("message", onMessage);
      try {
        socket.close();
      } catch {
        // already closed
      }
      reject(err);
    };
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      socket.off("error", onError);
      socket.off("message", onMessage);
      socket.close();
      reject(new Error("timed out waiting for udp stream"));
    }, timeoutMs);
    const onMessage = (msg: Buffer, rinfo: RemoteInfo) => {
      if (msg.length < 4 || msg.readUInt32BE(0) !== UDP_CONNECT_MSG) return;
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.off("error", onError);
      socket.off("message", onMessage);
      socket.connect(rinfo.port, rinfo.address, () => {
        const reply = Buffer.alloc(4);
        reply.writeUInt32BE(UDP_CONNECT_REPLY, 0);
        socket.send(reply);
        resolve(socket);
      });
    };

    socket.on("error", onError);
    socket.on("message", onMessage);
  });
}
