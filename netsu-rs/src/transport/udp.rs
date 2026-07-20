//! UDP data plane: the iperf3 stream-setup handshake, the per-packet header,
//! token-bucket pacing, the send-capability probe, and the sender/receiver
//! loops. Ported from `packages/netsu/src/transport/udp.ts`; see `PROTOCOL.md`
//! "UDP specifics".
//!
//! Unlike TCP, UDP is packet-based and does not go through the `DataChannel`
//! byte-stream trait — the client and server drive these functions directly
//! with a connected [`tokio::net::UdpSocket`].

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand::Rng;
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::error::{NetsuError, Result};
use crate::stats::JitterTracker;
use crate::streams::runner::{SharedCounters, SharedMeter};

/// iperf3 stream-setup magic values, as **raw wire bytes** (not integers).
///
/// `iperf.h` defines these per host endianness so that a raw, un-byte-swapped
/// `write()` of a host-native `unsigned int` puts the *same* bytes on the wire
/// regardless of host endianness. Phase 1's TypeScript and an early PROTOCOL.md
/// both had these byte-swapped (as big-endian integers), which failed against
/// the real binary; PROTOCOL.md is now corrected to match these bytes, verified
/// against real iperf3 3.21. Declaring them `[u8; 4]` rather than `u32` makes
/// that endianness bug unrepresentable here — do not "simplify" them back into
/// integers.
pub const UDP_CONNECT_MSG: [u8; 4] = [0x39, 0x38, 0x37, 0x36]; // ASCII "9876"
pub const UDP_CONNECT_REPLY: [u8; 4] = [0x36, 0x37, 0x38, 0x39]; // ASCII "6789"
pub const LEGACY_UDP_CONNECT_REPLY: [u8; 4] = [0xB1, 0x68, 0xDE, 0x3A];

/// Packet header: `sec(u32 BE) | usec(u32 BE) | pcount(u32 BE)`; the rest of
/// the datagram is filler.
pub const UDP_HEADER_SIZE: usize = 12;

/// Returned by [`probe_max_udp_send_len`] when *nothing* is sendable — not even
/// a bare `UDP_HEADER_SIZE` datagram. Distinct from every real probe result
/// (all `>= UDP_HEADER_SIZE`), so a caller can tell "clamped to N" apart from
/// "this host cannot send UDP at all" and refuse the test up front rather than
/// proceeding at an untested size.
pub const UDP_SEND_UNAVAILABLE: usize = 0;

/// Client-hello timeout: iperf3 sends its hello exactly once, so a lost hello
/// just times out here rather than retrying.
const UDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Wall-clock microseconds since the Unix epoch. Both the sender's embedded
/// timestamp and the receiver's arrival time use this same clock domain; the
/// two are different hosts in general, so [`JitterTracker`] does the
/// clock-skew-safe signed subtraction.
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Writes the 12-byte header at offset 0 of `buf`. `now_micros` is split into
/// whole seconds and the microsecond remainder, matching iperf3's `sec`/`usec`
/// fields. `buf` must be at least `UDP_HEADER_SIZE` long.
pub fn write_udp_header(buf: &mut [u8], pcount: u32, now_micros: u64) {
    let sec = (now_micros / 1_000_000) as u32;
    let usec = (now_micros % 1_000_000) as u32;
    buf[0..4].copy_from_slice(&sec.to_be_bytes());
    buf[4..8].copy_from_slice(&usec.to_be_bytes());
    buf[8..12].copy_from_slice(&pcount.to_be_bytes());
}

/// Reads the header, returning `(pcount, sent_micros)`. Errors if `buf` is
/// shorter than the header.
pub fn read_udp_header(buf: &[u8]) -> Result<(u32, u64)> {
    if buf.len() < UDP_HEADER_SIZE {
        return Err(NetsuError::Protocol(format!(
            "udp datagram too short for header: {} < {UDP_HEADER_SIZE}",
            buf.len()
        )));
    }
    // Indices 0..12 are in range (the length check above guarantees it), so
    // these array literals cannot panic and keep library code `expect`-free.
    let sec = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let usec = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let pcount = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let sent_micros = sec as u64 * 1_000_000 + usec as u64;
    Ok((pcount, sent_micros))
}

/// Token-bucket pacing with a bounded burst. `gate(bits)` accounts `bits`
/// against the configured rate and resolves once that many bits' worth of time
/// has actually elapsed since construction, smoothing a tight send loop to the
/// target bitrate. `rate == 0` disables pacing (iperf3's `-b 0` "unlimited",
/// which a remote peer can request).
///
/// `gate` ALWAYS yields to the runtime before returning, including on the
/// unpaced and no-sleep-needed paths. Phase 1's TypeScript version returned
/// early on the unpaced path, and the send loop — which has no other await —
/// spun at ~99% CPU forever, never reading the control channel's TEST_END and
/// deaf to shutdown, reachable by any peer sending `iperf3 -u -b 0 -R`. In
/// tokio the equivalent trap is a loop that only ever completes ready futures;
/// `tokio::task::yield_now()` on the no-sleep branch forces the runtime to poll
/// other tasks (the control loop, the shutdown watch) every iteration.
pub struct Pacer {
    rate: u64, // bits/s; 0 = unpaced
    start: Instant,
    bits_sent: u64,
    burst_cap: Duration,
}

impl Pacer {
    pub fn new(bits_per_second: u64) -> Self {
        Pacer {
            rate: bits_per_second,
            start: Instant::now(),
            bits_sent: 0,
            burst_cap: Duration::from_millis(100),
        }
    }

    pub async fn gate(&mut self, bits: u64) {
        self.bits_sent += bits;
        if self.rate == 0 {
            tokio::task::yield_now().await;
            return;
        }
        let ideal = Duration::from_secs_f64(self.bits_sent as f64 / self.rate as f64);
        let elapsed = self.start.elapsed();
        // Drifted behind schedule by more than the burst cap (e.g. a runtime
        // stall)? Pull the virtual start forward so only `burst_cap` of backlog
        // remains, bounding the catch-up burst to `burst_cap` worth of data.
        if elapsed > ideal + self.burst_cap
            && let Some(new_start) = Instant::now()
                .checked_sub(ideal)
                .and_then(|t| t.checked_sub(self.burst_cap))
        {
            self.start = new_start;
        }
        let elapsed = self.start.elapsed();
        if ideal > elapsed {
            let ahead = ideal - elapsed;
            if ahead > Duration::from_millis(1) {
                tokio::time::sleep(ahead).await;
                return;
            }
        }
        tokio::task::yield_now().await;
    }
}

/// Best-effort: raise `sock`'s send-buffer so a `want_bytes`-sized datagram can
/// be handed to the OS. iperf3 negotiates `len` from the path MTU (16332 on a
/// 16384-MTU loopback), above macOS's default per-socket UDP send ceiling
/// (`net.inet.udp.maxdgram`, 9216). Silent on failure — [`probe_max_udp_send_len`]
/// is the actual source of truth for what can be sent, not this.
pub fn try_raise_udp_send_buffer(sock: &UdpSocket, want_bytes: usize) {
    let r = SockRef::from(sock);
    let _ = r.set_send_buffer_size(want_bytes.max(65536));
}

/// The largest datagram actually emittable on this host, determined by a real
/// send on a private loopback socket (bound and connected to itself, never the
/// stream socket — a probe can never appear on the wire or touch a stream's
/// counters). Returns `requested` if that size already sends, a smaller clamped
/// size via binary search otherwise, or [`UDP_SEND_UNAVAILABLE`] when not even a
/// bare header-sized datagram can be sent. Falls back to `requested` if the
/// probe socket itself can't be set up — this must never hang a test.
pub async fn probe_max_udp_send_len(requested: usize) -> usize {
    if requested <= UDP_HEADER_SIZE {
        return requested;
    }
    let sock = match UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await {
        Ok(s) => s,
        Err(_) => return requested,
    };
    let local = match sock.local_addr() {
        Ok(a) => a,
        Err(_) => return requested,
    };
    if sock.connect(local).await.is_err() {
        return requested;
    }
    try_raise_udp_send_buffer(&sock, requested * 2);

    let can_send = |n: usize| {
        let sock = &sock;
        async move {
            let zeros = vec![0u8; n];
            sock.send(&zeros).await.is_ok()
        }
    };

    if can_send(requested).await {
        return requested;
    }
    // Confirm the floor explicitly before trusting it as a lower bound: if
    // nothing at all is sendable, the search below never runs and would
    // otherwise return an untested `UDP_HEADER_SIZE`.
    if !can_send(UDP_HEADER_SIZE).await {
        return UDP_SEND_UNAVAILABLE;
    }
    let mut lo = UDP_HEADER_SIZE;
    let mut hi = requested;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if can_send(mid).await {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Client side of iperf3's UDP stream setup: send the hello from a fresh
/// socket, wait for the reply (accepting the legacy value), then `connect()` so
/// the kernel pins the 4-tuple. 5s timeout, no retry (matches iperf3).
pub async fn udp_client_connect(host: &str, port: u16) -> Result<UdpSocket> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    sock.connect((host, port)).await?;
    sock.send(&UDP_CONNECT_MSG).await?;

    let mut buf = [0u8; 64];
    tokio::time::timeout(UDP_CONNECT_TIMEOUT, async {
        loop {
            let n = sock.recv(&mut buf).await?;
            if n >= 4 && (buf[0..4] == UDP_CONNECT_REPLY || buf[0..4] == LEGACY_UDP_CONNECT_REPLY) {
                return Ok::<(), NetsuError>(());
            }
            // Stray datagram; keep waiting until the deadline.
        }
    })
    .await
    .map_err(|_| NetsuError::Timeout)??;
    Ok(sock)
}

/// Binds a stream-accept socket on the shared UDP port with address/port reuse,
/// so a fresh listener can bind while earlier (now connected) stream sockets
/// keep the port — the kernel routes each pinned 4-tuple to its own connected
/// socket and any unmatched datagram (a fresh hello) to this listener.
///
/// The FIRST bind of a test must complete before CREATE_STREAMS is announced:
/// iperf3 clients send their hello exactly once, immediately on seeing
/// CREATE_STREAMS, so a bind that races the announce can drop the hello and
/// hang the test.
pub async fn udp_server_bind(port: u16) -> Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    // BSD/macOS routes multiple bound sockets on one port by 4-tuple only with
    // SO_REUSEPORT as well; harmless on Linux where SO_REUSEADDR suffices.
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    // Bind all interfaces, not just loopback: for cross-host (and
    // cross-container, e.g. the interop matrix) UDP the datagrams arrive on a
    // non-loopback address. iperf3's server binds the wildcard too.
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
    sock.bind(&addr.into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

/// Server side of iperf3's UDP stream setup, connect phase only: wait for the
/// hello on a bound socket, `connect()` to the sender (pinning the 4-tuple),
/// and return the same (now connected) socket. Does NOT send the reply — the
/// caller sends it after binding the next stream's listener, closing the window
/// where a fast client's next hello finds nothing bound (see
/// [`udp_server_send_reply`]).
pub async fn udp_server_accept(sock: UdpSocket, timeout: Duration) -> Result<UdpSocket> {
    let mut buf = [0u8; 64];
    let peer = tokio::time::timeout(timeout, async {
        loop {
            let (n, peer) = sock.recv_from(&mut buf).await?;
            if n >= 4 && buf[0..4] == UDP_CONNECT_MSG {
                return Ok::<SocketAddr, NetsuError>(peer);
            }
            // Stray datagram on the listen socket; ignore and keep waiting.
        }
    })
    .await
    .map_err(|_| NetsuError::Timeout)??;
    sock.connect(peer).await?;
    Ok(sock)
}

/// Sends the reply on an already-`connect()`ed stream socket.
pub async fn udp_server_send_reply(sock: &UdpSocket) -> Result<()> {
    sock.send(&UDP_CONNECT_REPLY).await?;
    Ok(())
}

/// UDP sender loop: pace, stamp a header, send, and account — until `shutdown`
/// fires. Clamps the datagram size to what this host can actually emit
/// ([`probe_max_udp_send_len`]); credits `bytes` only for datagrams the OS
/// accepted, counts a send error rather than aborting (matching iperf3), and
/// advances `pcount` per *attempt* so the receiver's loss accounting stays
/// correct across a locally-known failure.
///
/// If nothing is sendable at all, the loop does not spin: it records a single
/// error and returns. The server's PARAM_EXCHANGE-time check normally refuses
/// such a test up front; this is the defensive fallback.
pub async fn run_udp_sender(
    sock: UdpSocket,
    counters: SharedCounters,
    meter: SharedMeter,
    requested_len: usize,
    bandwidth: u64,
    mut shutdown: watch::Receiver<bool>,
) {
    try_raise_udp_send_buffer(&sock, requested_len * 2);
    let len = probe_max_udp_send_len(requested_len).await;
    // `< UDP_HEADER_SIZE` covers both UDP_SEND_UNAVAILABLE (0) and a negotiated
    // `len` of 4..=11: params allows `len >= 4`, but a datagram smaller than the
    // 12-byte header can't carry one — allocating `vec![0u8; len]` and then
    // indexing `buf[UDP_HEADER_SIZE..]` / writing the header would panic. Treat
    // it as unsendable (counted error, no send) rather than panicking the task.
    if len < UDP_HEADER_SIZE {
        eprintln!(
            "netsu: cannot send a UDP datagram of {len} byte(s) (< {UDP_HEADER_SIZE}-byte header) — refusing to send"
        );
        counters.lock().await.errors += 1;
        return;
    }
    if len < requested_len {
        eprintln!(
            "netsu: udp len {requested_len} exceeds the largest datagram this host can send \
             ({len} bytes); sending {len}-byte datagrams instead"
        );
    }

    let mut buf = vec![0u8; len];
    // Randomize the filler once (defeats link compression, matching iperf3);
    // only the 12-byte header is rewritten per packet.
    rand::thread_rng().fill(&mut buf[UDP_HEADER_SIZE..]);
    let mut pacer = Pacer::new(bandwidth);
    let mut pcount: u32 = 0;

    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            _ = pacer.gate(len as u64 * 8) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        pcount = pcount.wrapping_add(1);
        write_udp_header(&mut buf, pcount, now_micros());
        match sock.send(&buf).await {
            Ok(_) => {
                let mut c = counters.lock().await;
                c.bytes += len as u64;
                c.packets = pcount as u64;
                drop(c);
                // Feed the interval meter so UDP live/--json per-second
                // throughput matches TCP/WS (and iperf3 / the TS impl), not just
                // the final summary. Credit only datagrams the kernel accepted.
                meter.lock().await.add(len as u64);
            }
            Err(_) => {
                let mut c = counters.lock().await;
                c.errors += 1;
                c.packets = pcount as u64;
            }
        }
    }
}

/// UDP receiver loop: read datagrams, feed each header to a [`JitterTracker`],
/// and accumulate received bytes — until `shutdown` fires or a read errors.
///
/// The iperf3 receiver-side summary is written into `counters` **per packet**,
/// not just on exit: `local_results` reads the counters during EXCHANGE_RESULTS
/// while a reverse-mode receiver is still running (it isn't torn down until
/// final teardown, per protocol fact 3), so a write-only-on-exit summary would
/// be read as all-zero. `packets` is the max sequence number seen (received +
/// lost, matching iperf3), `errors` is the lost count, `jitter` the RFC 1889
/// estimate in seconds. Since the sending peer has already stopped by the time
/// results are exchanged, the running values are effectively final by then.
///
/// Like the TCP receiver, this must be given a real shutdown signal: `recv` has
/// no timeout of its own, so on an idle-but-open socket it would otherwise never
/// return.
pub async fn run_udp_receiver(
    sock: UdpSocket,
    counters: SharedCounters,
    meter: SharedMeter,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tracker = JitterTracker::new();
    let mut buf = vec![0u8; 65536];
    loop {
        if *shutdown.borrow() {
            break;
        }
        let n = tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            result = sock.recv(&mut buf) => match result {
                Ok(n) => n,
                Err(_) => break,
            }
        };
        if n < UDP_HEADER_SIZE {
            continue;
        }
        if let Ok((pcount, sent_micros)) = read_udp_header(&buf[..n]) {
            tracker.on_packet(pcount, sent_micros, now_micros());
            let mut c = counters.lock().await;
            c.bytes += n as u64;
            c.packets = tracker.max_seq() as u64;
            c.errors = tracker.lost();
            c.jitter = tracker.jitter_secs();
            drop(c);
            // Feed the interval meter so the client's per-second UDP receive
            // throughput isn't reported as zero (see run_udp_sender).
            meter.lock().await.add(n as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_constants_are_the_documented_wire_bytes() {
        assert_eq!(&UDP_CONNECT_MSG, b"9876");
        assert_eq!(&UDP_CONNECT_REPLY, b"6789");
    }

    #[test]
    fn packet_header_round_trips_pcount_and_timestamp() {
        let mut buf = vec![0u8; 64];
        write_udp_header(&mut buf, 42, 1_500_000_123_456);
        let (pcount, sent) = read_udp_header(&buf).unwrap();
        assert_eq!(pcount, 42);
        // Second resolution on sec + microsecond remainder.
        assert_eq!(sent / 1_000_000, 1_500_000_123_456 / 1_000_000);
    }

    #[tokio::test]
    async fn pacer_holds_about_1mbit_per_second() {
        let mut p = Pacer::new(1_000_000);
        let start = std::time::Instant::now();
        for _ in 0..25 {
            p.gate(5000).await; // 25 x 5000 bits = 125_000 bits = 0.125s at 1Mbit
        }
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(90),
            "elapsed {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn unpaced_gate_still_yields() {
        // rate 0 means unpaced; `gate` must still return promptly (not sleep)
        // and must not error. This does NOT by itself prove `gate` yields to the
        // runtime — a non-yielding `gate` would also complete this finite loop;
        // the actual anti-livelock guard is the end-to-end
        // `unpaced_reverse_udp_does_not_livelock_the_server` interop test. This
        // just pins that the unpaced path completes a tight loop quickly.
        let mut p = Pacer::new(0);
        for _ in 0..10_000 {
            p.gate(12_000).await;
        }
    }
}
