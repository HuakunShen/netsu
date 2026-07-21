//! Client control state machine: drives one test session against a real
//! iperf3 (or netsu) server over the TCP control channel, per `PROTOCOL.md`'s
//! "Test lifecycle". Ported from `packages/netsu/src/client.ts` — see that
//! file's comments for the TEST_END ordering, the reverse-mode teardown
//! race, and the EXCHANGE_RESULTS idempotence fix this module also honors.
//!
//! ## Transport dispatch: generics/monomorphization, not an enum wrapper
//!
//! **This section is about the client specifically — see the callout at the
//! end before applying this reasoning to the server.** `protocol::pipe::BytePipe`
//! uses native async-fn-in-trait and is therefore **not** dyn-compatible
//! (`Box<dyn BytePipe>` does not compile — confirmed against this repo's
//! pinned rustc). Only one control-channel transport (`TcpPipe`) exists in
//! this task, so there is exactly one call site and no polymorphism to
//! resolve yet. When Task 9 adds a WS control channel, the natural extension
//! *for the client* is to add a sibling concrete function (e.g. `run_ws`)
//! next to [`run_tcp`] — both calling the same generic protocol helpers
//! (`read_state<P: BytePipe>`, `write_json<P: BytePipe>`, ...) that already
//! exist. That is monomorphization: each transport gets the compiler to
//! generate its own specialized copy of the shared logic, with zero
//! dispatch-table boilerplate. The alternative — an `enum Pipe { Tcp(..),
//! Ws(..) }` wrapper implementing `BytePipe` by hand-written `match` per
//! method — is the only way to make a *single* function generic over "either
//! transport" without dyn dispatch, but writing that dispatch shell for a
//! trait with exactly one live implementor buys nothing today; the Rule of
//! Three says wait for the second implementor (Task 9) before generalizing.
//! This works cleanly for the client because [`Session`] never stores the
//! pipe: `run_loop` takes `control: &mut TcpPipe` as a parameter, so a
//! sibling `run_ws` just calls the same `Session` methods with a different
//! concrete pipe type threaded through as an argument — no duplicated state
//! machine.
//!
//! **Do not copy the "sibling concrete function" conclusion to the server.**
//! Task 7's `ServerCore::handle_connection` *is* the state machine (unlike
//! here, where `Session` and the transport are separate) — a sibling
//! `handle_connection_ws` next to a concrete `handle_connection` would
//! duplicate that entire state machine, exactly what the Rule of Three
//! argument above is trying to avoid, not what it recommends. For the
//! server, write `handle_connection` generic from the start —
//! `async fn handle_connection<P: BytePipe>(..)` — so each accept loop
//! monomorphizes it with its own concrete pipe type; no heterogeneous
//! storage is needed since a single connection's pipe type is known at its
//! call site, so this isn't the same "one call site, no polymorphism yet"
//! situation the client is in.
//!
//! Data streams are different: [`crate::streams::channel::DataChannel`] is
//! `#[async_trait]`, so it *is* dyn-compatible, and this module already
//! stores every open stream as `Box<dyn DataChannel>` behind a shared
//! `Arc<Mutex<..>>` (see `streams::runner`). That single representation
//! already covers TCP now and will cover WS/UDP later without any change to
//! the stream bookkeeping in [`Session`] — only the `open_stream` factory
//! function needs a transport-specific twin.

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio::time::{self, Interval, Sleep};

use crate::error::{NetsuError, Result};
use crate::protocol::cookie::{cookie_to_bytes, make_cookie};
use crate::protocol::framing::{MAX_JSON, read_json, read_state, write_json, write_state};
use crate::protocol::params::{
    self, DEFAULT_TCP_LEN, DEFAULT_UDP_BANDWIDTH, DEFAULT_UDP_LEN, TestParams,
};
use crate::protocol::pipe::BytePipe;
use crate::protocol::results::{self, EndResults};
use crate::protocol::states::{
    ACCESS_DENIED, COOKIE_SIZE, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS, IPERF_DONE,
    IPERF_START, PARAM_EXCHANGE, SERVER_ERROR, TEST_END, TEST_RUNNING, TEST_START,
};
use crate::stats::{IntervalReport, bits_per_second};
use crate::streams::runner::{
    SharedChannel, SharedCounters, SharedMeter, StreamCounters, next_stream_id, run_receiver,
    run_sender,
};
#[cfg(feature = "quic")]
use crate::transport::quic::endpoint::STREAMS_TIMEOUT as QUIC_STREAMS_TIMEOUT;
use crate::transport::tcp::{CONNECT_TIMEOUT, TcpPipe};
use crate::transport::udp::{run_udp_receiver, run_udp_sender, udp_client_connect};
#[cfg(feature = "ws")]
use crate::transport::ws::{WS_CONNECT_TIMEOUT, WsPipe};
use tokio::net::UdpSocket;

/// Control-channel timeout outside `TEST_RUNNING` (30s, matches
/// `PROTOCOL.md`'s "Control-channel timeouts"). While running, the timeout
/// is `duration + CONTROL_TIMEOUT` so a slow-but-legitimate test doesn't trip
/// it (mirrors `client.ts`'s `CONTROL_TIMEOUT` usage).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// Which control-channel (and, for now, data-stream) transport to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    #[default]
    Tcp,
    #[cfg(feature = "ws")]
    Ws,
    /// One iroh/QUIC connection carrying the control stream + all data streams.
    #[cfg(feature = "iroh")]
    Iroh,
    /// Fixed-address Quinn transport with explicit TLS trust configuration.
    #[cfg(feature = "quic")]
    Quic,
}

/// Client trust configuration for the native QUIC transport.
#[cfg(feature = "quic")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuicClientOptions {
    /// Explicit benchmark-only certificate verification bypass.
    pub insecure: bool,
    /// PEM file containing the CA used to authenticate the server.
    pub ca_path: Option<std::path::PathBuf>,
}

/// Client-side test configuration.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub port: u16,
    pub transport: Transport,
    pub udp: bool,
    pub reverse: bool,
    pub duration: u32,
    pub parallel: u32,
    pub len: Option<usize>,
    pub bandwidth: Option<u64>,
    pub interval: Option<Duration>,
    /// iroh only: bind a direct-only endpoint (no relay/discovery) and fail the
    /// run if the selected path is a relay. Ignored by TCP/UDP/WS.
    pub direct_only: bool,
    /// Native QUIC-only trust configuration.
    #[cfg(feature = "quic")]
    pub quic: Option<QuicClientOptions>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            port: 5201,
            transport: Transport::Tcp,
            udp: false,
            reverse: false,
            duration: 10,
            parallel: 1,
            len: None,
            bandwidth: None,
            interval: Some(Duration::from_secs(1)),
            direct_only: false,
            #[cfg(feature = "quic")]
            quic: None,
        }
    }
}

impl ClientOptions {
    /// Rejects contradictory transport-specific options before network I/O.
    pub fn validate(&self) -> Result<()> {
        #[cfg(feature = "quic")]
        {
            if self.transport == Transport::Quic {
                if self.udp {
                    return Err(NetsuError::Protocol(
                        "UDP mode is mutually exclusive with native QUIC".into(),
                    ));
                }
                let quic = self
                    .quic
                    .as_ref()
                    .ok_or_else(|| NetsuError::Protocol("missing QUIC client options".into()))?;
                if quic.insecure == quic.ca_path.is_some() {
                    return Err(NetsuError::Protocol(
                        "QUIC client requires exactly one of insecure or CA path".into(),
                    ));
                }
            } else if self.quic.is_some() {
                return Err(NetsuError::Protocol(
                    "QUIC client options require Transport::Quic".into(),
                ));
            }
        }
        Ok(())
    }
}

/// UDP-only summary stats, `None` for TCP tests.
#[derive(Debug, Clone)]
pub struct UdpStats {
    pub jitter_secs: f64,
    pub lost: u64,
    pub packets: u64,
    pub lost_percent: f64,
}

/// A snapshot of an iroh connection's selected path, for the result JSON. Plain
/// data (no iroh types) so it lives in the always-compiled [`TestResult`];
/// populated by `p2p::observe` only for `Transport::Iroh`.
#[derive(Debug, Clone)]
pub struct IrohConnectionInfo {
    /// `"direct"`, `"relay"`, or `"unknown"`.
    pub observed_path: String,
    /// Selected-path RTT in microseconds, if a path is selected.
    pub rtt_us: Option<u64>,
    pub remote_addr: Option<String>,
}

/// Diagnostics captured from a completed native QUIC connection.
#[cfg(feature = "quic")]
#[derive(Debug, Clone)]
pub struct QuicConnectionInfo {
    pub handshake_ms: f64,
    pub rtt_us: Option<u64>,
    pub remote_addr: Option<String>,
    pub certificate_verification: &'static str,
    pub lost_packets: Option<u64>,
    pub congestion_events: Option<u64>,
}

/// Transport-specific connection diagnostics attached to a completed test.
#[derive(Debug, Clone)]
pub enum ConnectionInfo {
    #[cfg(feature = "iroh")]
    Iroh(IrohConnectionInfo),
    #[cfg(feature = "quic")]
    Quic(QuicConnectionInfo),
    #[cfg(not(any(feature = "iroh", feature = "quic")))]
    Unavailable,
}

/// Stable JSON representation for transport-specific diagnostics.
///
/// Native QUIC addresses are intentionally omitted by default. Iroh retains
/// its historical keys so existing consumers do not lose fields during the
/// migration to the shared envelope.
pub fn connection_json(info: &ConnectionInfo) -> serde_json::Value {
    match info {
        #[cfg(feature = "iroh")]
        ConnectionInfo::Iroh(info) => serde_json::json!({
            "transport": "iroh",
            "observed_path": info.observed_path,
            "rtt_us": info.rtt_us,
            "remote_addr": info.remote_addr,
        }),
        #[cfg(feature = "quic")]
        ConnectionInfo::Quic(info) => serde_json::json!({
            "transport": "quic",
            "path": "direct",
            "handshake_ms": info.handshake_ms,
            "rtt_us": info.rtt_us,
            "certificate_verification": info.certificate_verification,
            "lost_packets": info.lost_packets,
            "congestion_events": info.congestion_events,
        }),
        #[cfg(not(any(feature = "iroh", feature = "quic")))]
        ConnectionInfo::Unavailable => serde_json::Value::Null,
    }
}

/// The finished test's results, from this client's point of view.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub udp: bool,
    pub reverse: bool,
    pub duration_seconds: f64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub send_bits_per_second: f64,
    pub receive_bits_per_second: f64,
    pub local: EndResults,
    pub remote: EndResults,
    pub udp_stats: Option<UdpStats>,
    /// Optional diagnostics for transports that expose connection metadata.
    pub connection: Option<ConnectionInfo>,
}

/// Runs one client test session against `host`, tearing down every socket
/// and task it opened before returning — on success or on error alike.
///
/// The transport chooses the control pipe ([`run_tcp`] / [`run_ws`]), both of
/// which drive the same generic [`run_control`] state machine. UDP is
/// data-plane only, so a UDP test still uses a TCP control channel; only its
/// data streams differ.
pub async fn run_client(
    host: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    opts.validate()?;
    match opts.transport {
        #[cfg(feature = "ws")]
        Transport::Ws => run_ws(host, opts, on_interval).await,
        #[cfg(feature = "iroh")]
        Transport::Iroh => run_iroh(host, opts, on_interval).await,
        #[cfg(feature = "quic")]
        Transport::Quic => run_quic(host, opts, on_interval).await,
        Transport::Tcp => run_tcp(host, opts, on_interval).await,
    }
}

/// The transport-specific half of a data stream. TCP and WS streams both speak
/// the `DataChannel` byte-stream trait (`Channel`); UDP streams are
/// packet-based and drive a raw [`UdpSocket`] directly. A forward-mode UDP
/// stream holds its socket here until [`Session::start_running`] moves it into
/// the sender task (receivers take the socket at open time, so their variant is
/// already `None`).
enum StreamIo {
    Channel(SharedChannel),
    Udp(Option<UdpSocket>),
}

/// One data stream's bookkeeping: the shared counters the spawned
/// sender-or-receiver task also holds, plus that task's handle so teardown
/// can join (or, failing that, abort) it.
struct StreamState {
    counters: SharedCounters,
    io: StreamIo,
    task: Option<JoinHandle<()>>,
    /// Whether `close()` has already run on this stream. This is what
    /// `handle_exchange_results` keys off of — *not* `latched_error.is_some()`
    /// — because `Option<String>` alone can't distinguish "not yet closed"
    /// from "closed, no error latched": a healthy stream closes with no
    /// error, which left `latched_error` at `None` in both cases. Without
    /// this flag, the healthy (and common) case fell through to a live read
    /// of the channel that was meant to be a fallback only, and for a
    /// reverse-mode stream (never closed at this point — see the
    /// `TEST_END` handling in `run_loop`) that live read raced a receiver
    /// task holding the channel's lock across a pending, unbounded
    /// `read_chunk().await` forever (`streams::runner::run_receiver`),
    /// hanging the whole control loop. `client.ts` never has this problem:
    /// its `finalize()` returns `transferError`, which is *only* ever
    /// assigned inside `close()`, guarded by its own `closed` boolean — it
    /// never live-reads. The live-read fallback here has been removed
    /// entirely to match; see `close`'s doc for what that means for
    /// never-closed streams.
    closed: bool,
    /// Snapshot of `channel.error()` taken at the moment we force-closed this
    /// stream (forward mode, duration timer), *before* calling `close()`.
    /// Mirrors `client.ts`'s `TcpDataChannel` doc: forcibly shutting down a
    /// socket while a sender task races it for the same lock can itself
    /// induce a fresh, self-inflicted write failure (e.g. a doomed write
    /// that slips in right after shutdown) — that teardown noise must not
    /// be reported as a genuine transfer failure. Reading `channel.error()`
    /// live from `handle_exchange_results`, after close() already ran, would
    /// pick up exactly that noise; consulting this pre-close snapshot
    /// instead avoids it. Only meaningful once `closed` is `true`.
    latched_error: Option<String>,
}

impl StreamState {
    /// Snapshots any error already latched on the channel, then closes it.
    /// Idempotent: closing twice (duration timer, then final teardown) is a
    /// no-op the second time — `closed` gates the whole body, so the
    /// snapshot is taken exactly once and the channel is never re-locked
    /// (or re-closed) afterward.
    ///
    /// A stream this method never runs for (e.g. a reverse-mode stream at
    /// EXCHANGE_RESULTS time, still receiving) reports no error to
    /// `handle_exchange_results` — deliberately: matching `client.ts`'s
    /// `finalize()`, which likewise only ever reports a value set inside
    /// `close()`. A never-closed stream's underlying `ECONNRESET` (say, from
    /// a peer that vanished mid-test) is therefore not surfaced as "data
    /// stream failed" here; it is instead observed, if at all, once final
    /// teardown actually closes the stream.
    async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        // UDP streams have no channel to close (the sender/receiver task stops
        // on its shutdown watch and drops the socket) and no fatal-error
        // concept — a UDP send error or lost packet is counted, never fatal —
        // so `latched_error` stays `None` for them, keeping them out of
        // `handle_exchange_results`'s "data stream failed" path.
        if let StreamIo::Channel(channel) = &self.io {
            let mut ch = channel.lock().await;
            self.latched_error = ch.error().map(|e| e.to_string());
            ch.close().await;
        }
    }
}

/// All mutable state for one client test session. Lives only inside
/// [`run_tcp`]; not part of the public interface.
struct Session {
    host: String,
    port: u16,
    transport: Transport,
    cookie: [u8; COOKIE_SIZE],
    params: TestParams,
    interval: Option<Duration>,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,

    /// The iroh connection whose bi-streams carry this session's data streams.
    /// `Some` only for `Transport::Iroh`; `open_stream` opens data streams on
    /// it instead of dialing a new connection per stream.
    #[cfg(feature = "iroh")]
    iroh_connection: Option<iroh::endpoint::Connection>,
    /// The native Quinn connection whose bi-streams carry this session.
    #[cfg(feature = "quic")]
    quic_connection: Option<quinn::Connection>,

    streams: Vec<StreamState>,
    meter: SharedMeter,
    /// Shared shutdown signal for forward-mode sender tasks only. Receiver
    /// tasks (reverse mode) never observe this — see protocol fact 3 in the
    /// module-level docs and `streams::runner::run_receiver`.
    stop_senders: watch::Sender<bool>,
    stop_senders_rx: watch::Receiver<bool>,
    /// Separate shutdown signal for reverse-mode receiver tasks, fired only
    /// from [`Session::teardown`] — never from the duration-timer's early
    /// TEST_END handling, which must leave receivers running (protocol fact
    /// 3). Without this, a receiver sitting in a pending, unbounded
    /// `read_chunk().await` on an idle-but-open socket (a half-open
    /// connection, a peer that stopped writing without closing) would hold
    /// the channel's mutex forever, and `StreamState::close`'s
    /// `self.channel.lock().await` in teardown would then hang right along
    /// with it — see `streams::runner::run_receiver`'s doc.
    stop_receivers: watch::Sender<bool>,
    stop_receivers_rx: watch::Receiver<bool>,

    running: bool,
    start_instant: Option<Instant>,
    end_instant: Option<Instant>,
    remote: Option<EndResults>,

    duration_sleep: Option<Pin<Box<Sleep>>>,
    ticker: Option<Interval>,
}

impl Session {
    fn new(
        host: String,
        port: u16,
        transport: Transport,
        cookie: [u8; COOKIE_SIZE],
        params: TestParams,
        interval: Option<Duration>,
        on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
    ) -> Self {
        let (stop_senders, stop_senders_rx) = watch::channel(false);
        let (stop_receivers, stop_receivers_rx) = watch::channel(false);
        Session {
            host,
            port,
            transport,
            cookie,
            params,
            interval,
            on_interval,
            #[cfg(feature = "iroh")]
            iroh_connection: None,
            #[cfg(feature = "quic")]
            quic_connection: None,
            streams: Vec::new(),
            meter: Arc::new(Mutex::new(crate::stats::IntervalMeter::new(Instant::now()))),
            stop_senders,
            stop_senders_rx,
            stop_receivers,
            stop_receivers_rx,
            running: false,
            start_instant: None,
            end_instant: None,
            remote: None,
            duration_sleep: None,
            ticker: None,
        }
    }

    /// The control loop: reads a state byte and dispatches, exactly as
    /// `client.ts`'s `for (;;)` does. Every exit — normal completion or any
    /// `?`-propagated error — is followed by [`Session::teardown`] in
    /// [`run_tcp`], which is the single teardown path.
    async fn run_loop<P: BytePipe>(&mut self, control: &mut P) -> Result<TestResult> {
        control.write_all(&self.cookie).await?;

        loop {
            let timeout = if self.running {
                Duration::from_secs(self.params.time as u64) + CONTROL_TIMEOUT
            } else {
                CONTROL_TIMEOUT
            };

            tokio::select! {
                biased;

                // Duration timer: fires once, `duration_sleep.take()`-style
                // idempotence via the `end_instant.is_none()` guard below —
                // once EXCHANGE_RESULTS sets `end_instant` first (protocol
                // fact 4), this arm is simply never selected again, so it
                // can neither double-fire TEST_END nor clobber `end_instant`
                // with a stale, later timestamp.
                _ = fire_if_armed(&mut self.duration_sleep), if self.running && self.end_instant.is_none() => {
                    self.end_instant = Some(Instant::now());
                    self.running = false;
                    // Real iperf3 signals end-of-test on the control channel
                    // FIRST, then tears down data fds (protocol fact 3):
                    // write TEST_END before closing any stream.
                    write_state(control, TEST_END).await?;
                    // Signal senders to stop *before* force-closing below:
                    // this lets a sender's `changed()` race intercept it
                    // ahead of its next write, so the common case is a clean
                    // stop rather than a doomed write against a socket we're
                    // about to shut down (see `StreamState::close`'s doc for
                    // why that residual race is still handled defensively).
                    let _ = self.stop_senders.send(true);
                    if !self.params.reverse {
                        for s in &mut self.streams {
                            s.close().await;
                        }
                    }
                    // In reverse mode we deliberately do NOT close the
                    // receive streams here: the server is still writing,
                    // driven by its own TEST_END handling, and destroying
                    // the socket out from under it would RST the sender
                    // mid-write. Final teardown (after EXCHANGE_RESULTS)
                    // closes them once the server has stopped on its own.
                }

                _ = tick_if_armed(&mut self.ticker), if self.running => {
                    if let Some(cb) = self.on_interval.as_mut() {
                        let report = self.meter.lock().await.snap(Instant::now());
                        cb(report);
                    }
                }

                state = read_state(control, Some(timeout)) => {
                    match state? {
                        IPERF_START | TEST_START => {} // informational, ignore
                        PARAM_EXCHANGE => {
                            write_json(control, &params::encode(&self.params)).await?;
                        }
                        CREATE_STREAMS => {
                            self.open_requested_streams().await?;
                        }
                        TEST_RUNNING => self.start_running().await,
                        EXCHANGE_RESULTS => self.handle_exchange_results(control).await?,
                        DISPLAY_RESULTS => {
                            write_state(control, IPERF_DONE).await?;
                            let local = self.local_results().await;
                            let remote = self.remote.clone().ok_or_else(|| {
                                NetsuError::Protocol("no results from server".into())
                            })?;
                            return Ok(self.build_test_result(local, remote));
                        }
                        ACCESS_DENIED => return Err(NetsuError::ServerBusy),
                        SERVER_ERROR => return Err(NetsuError::ServerError),
                        other => {
                            return Err(NetsuError::Protocol(format!(
                                "unexpected control state {other}"
                            )));
                        }
                    }
                }
            }
        }
    }

    /// Opens one data stream, assigns it the next iperf3-quirky id, and — for
    /// reverse mode only — attaches its receiver task immediately (matching
    /// `client.ts`'s `#openTcpStream`, which attaches receivers at stream-open
    /// time; forward-mode senders instead wait for `start_running`, which fires
    /// on `TEST_RUNNING`).
    async fn open_stream(&mut self) -> Result<()> {
        let id = next_stream_id(self.streams.len());
        let counters: SharedCounters = Arc::new(Mutex::new(StreamCounters::new(id)));

        let (io, task) = if self.params.udp {
            // The UDP hello must go out now (on CREATE_STREAMS) — the server is
            // waiting for it. A reverse-mode stream's receiver takes the socket
            // immediately; a forward-mode stream holds it until start_running.
            let socket = udp_client_connect(&self.host, self.port).await?;
            if self.params.reverse {
                let task = tokio::spawn(run_udp_receiver(
                    socket,
                    counters.clone(),
                    self.meter.clone(),
                    self.stop_receivers_rx.clone(),
                ));
                (StreamIo::Udp(None), Some(task))
            } else {
                (StreamIo::Udp(Some(socket)), None)
            }
        } else {
            let channel = match self.transport {
                #[cfg(feature = "ws")]
                Transport::Ws => open_ws_stream(&self.host, self.port, &self.cookie).await?,
                #[cfg(feature = "iroh")]
                Transport::Iroh => {
                    let conn = self.iroh_connection.as_ref().ok_or_else(|| {
                        NetsuError::Protocol("iroh data stream without a connection".into())
                    })?;
                    open_iroh_stream(conn, &self.cookie).await?
                }
                #[cfg(feature = "quic")]
                Transport::Quic => {
                    let connection = self.quic_connection.as_ref().ok_or_else(|| {
                        NetsuError::Protocol("QUIC data stream without a connection".into())
                    })?;
                    open_quic_stream(connection, &self.cookie).await?
                }
                Transport::Tcp => open_tcp_stream(&self.host, self.port, &self.cookie).await?,
            };
            let channel: SharedChannel = Arc::new(Mutex::new(channel));
            let task = if self.params.reverse {
                Some(tokio::spawn(run_receiver(
                    channel.clone(),
                    counters.clone(),
                    self.meter.clone(),
                    self.stop_receivers_rx.clone(),
                )))
            } else {
                None
            };
            (StreamIo::Channel(channel), task)
        };

        self.streams.push(StreamState {
            counters,
            io,
            task,
            closed: false,
            latched_error: None,
        });
        Ok(())
    }

    async fn open_requested_streams(&mut self) -> Result<()> {
        let parallel = self.params.parallel;
        #[cfg(feature = "quic")]
        let is_quic = self.transport == Transport::Quic;
        let open = async {
            for _ in 0..parallel {
                self.open_stream().await?;
            }
            Ok(())
        };
        #[cfg(feature = "quic")]
        if is_quic {
            return tokio::time::timeout(QUIC_STREAMS_TIMEOUT, open)
                .await
                .map_err(|_| NetsuError::Setup {
                    transport: "quic",
                    phase: crate::error::SetupPhase::ChannelsOpen,
                    detail: format!("timed out after {} seconds", QUIC_STREAMS_TIMEOUT.as_secs()),
                })?;
        }
        open.await
    }

    async fn start_running(&mut self) {
        self.running = true;
        let now = Instant::now();
        self.start_instant = Some(now);
        // Reset the *existing* meter in place rather than reallocating a new
        // `Arc`: reverse-mode receiver tasks were spawned back at
        // CREATE_STREAMS, each holding a clone of this same `Arc`. Only
        // forward-mode senders are spawned after this point (below), so a
        // fresh `Arc` here would orphan every receiver on the stale meter —
        // its interval reports would stay at zero for the rest of the test,
        // since the ticker (in `run_loop`) would be snapping the new meter
        // while receivers kept feeding the old one. `client.ts` avoids this
        // because its `attachReceiver(channel, counters, (n) =>
        // this.#meter.add(n))` closure re-reads `this.#meter` on every call,
        // so a later `this.#meter = new IntervalMeter(...)` reassignment is
        // picked up automatically; an eagerly-cloned `Arc` has no such
        // late-binding, so the fix here is to keep one `Arc` for the whole
        // session and mutate what it points to instead.
        *self.meter.lock().await = crate::stats::IntervalMeter::new(now);
        self.duration_sleep = Some(Box::pin(time::sleep(Duration::from_secs(
            self.params.time as u64,
        ))));
        self.ticker = match (self.on_interval.is_some(), self.interval) {
            (true, Some(d)) if d > Duration::ZERO => {
                Some(time::interval_at(time::Instant::now() + d, d))
            }
            _ => None,
        };

        if !self.params.reverse {
            let len = self.params.len;
            let bandwidth = self.params.bandwidth;
            let meter = self.meter.clone();
            for s in &mut self.streams {
                let rx = self.stop_senders_rx.clone();
                s.task = Some(match &mut s.io {
                    StreamIo::Channel(channel) => tokio::spawn(run_sender(
                        channel.clone(),
                        s.counters.clone(),
                        meter.clone(),
                        len,
                        rx,
                    )),
                    StreamIo::Udp(socket) => {
                        // Take the socket held since open_stream; it's always
                        // present for a forward-mode UDP stream at this point.
                        match socket.take() {
                            Some(sock) => tokio::spawn(run_udp_sender(
                                sock,
                                s.counters.clone(),
                                meter.clone(),
                                len,
                                bandwidth,
                                rx,
                            )),
                            None => continue,
                        }
                    }
                });
            }
        }
    }

    async fn handle_exchange_results<P: BytePipe>(&mut self, control: &mut P) -> Result<()> {
        // Idempotent with the duration timer (protocol fact 4): a
        // server-driven early EXCHANGE_RESULTS must not produce a negative
        // end_time. Guarding the duration-timer select arm on
        // `end_instant.is_none()` (in run_loop) makes disarming it automatic
        // once this fires first — that arm simply stops being selected.
        if self.end_instant.is_none() {
            self.end_instant = Some(Instant::now());
            self.running = false;
            let _ = self.stop_senders.send(true);
        }

        for s in &self.streams {
            // Only a stream we ourselves closed can have a meaningful error
            // here — see `StreamState::close`'s doc. There is deliberately no
            // live-read fallback for a stream that isn't closed yet (reverse
            // mode, still receiving; or an early EXCHANGE_RESULTS racing
            // ahead of our own duration timer): a live read would race the
            // receiver task holding the channel's lock across a pending,
            // unbounded read forever, hanging the whole control loop
            // (`streams::runner::run_receiver`) — exactly the bug this
            // snapshot-only check replaces.
            if s.closed
                && let Some(err) = s.latched_error.as_ref()
            {
                return Err(NetsuError::Protocol(format!("data stream failed: {err}")));
            }
        }

        let local = self.local_results().await;
        write_json(control, &results::encode(&local)).await?;
        let remote_json: serde_json::Value =
            read_json(control, MAX_JSON, Some(CONTROL_TIMEOUT)).await?;
        self.remote = Some(results::decode(remote_json)?);
        Ok(())
    }

    /// `&mut self`, not `&self`, even though the body only reads: an `async
    /// fn` taking `&self` captures a *shared* borrow of `Session` in the
    /// future it returns, and for that future to be `Send` (required
    /// whenever the caller's own future — e.g. `run_client`'s — is awaited
    /// inside a `tokio::spawn`ed task) the compiler needs `Session: Sync`.
    /// `Session` holds `on_interval: Option<Box<dyn FnMut(IntervalReport) +
    /// Send>>`, which is deliberately `Send`-only (an `FnMut` callback has no
    /// reason to require `Sync` from ordinary callers), so `Session` itself
    /// is `Send` but not `Sync`. Taking `&mut self` instead captures a
    /// *mutable* borrow, whose `Send`-ness only needs `Session: Send` — true
    /// here — sidestepping the `Sync` requirement entirely. Every call site
    /// already holds `&mut Session`, so this costs nothing.
    async fn local_results(&mut self) -> EndResults {
        let sender = !self.params.reverse;
        let end_seconds = match (self.start_instant, self.end_instant) {
            (Some(start), Some(end)) => end.duration_since(start).as_secs_f64(),
            _ => 0.0,
        };
        let mut streams = Vec::with_capacity(self.streams.len());
        for s in &self.streams {
            let c = s.counters.lock().await;
            streams.push(results::StreamResult {
                id: c.id,
                bytes: c.bytes,
                retransmits: -1, // no TCP_INFO plumbing in this phase
                jitter: c.jitter,
                errors: c.errors,
                packets: c.packets,
                start_time: 0.0,
                end_time: end_seconds,
            });
        }
        EndResults {
            sender_has_retransmits: if sender { 0 } else { -1 },
            streams,
        }
    }

    fn build_test_result(&self, local: EndResults, remote: EndResults) -> TestResult {
        let duration = match (self.start_instant, self.end_instant) {
            (Some(s), Some(e)) => e.duration_since(s).as_secs_f64(),
            _ => 0.0,
        };
        let sum = |r: &EndResults| r.streams.iter().map(|s| s.bytes).sum::<u64>();
        let sender = !self.params.reverse;
        let sent_bytes = if sender { sum(&local) } else { sum(&remote) };
        let received_bytes = if sender { sum(&remote) } else { sum(&local) };

        let udp_stats = if self.params.udp {
            // The receiver side carries the loss/jitter accounting: the remote
            // (server) when we send, the local (client) when we receive. Our
            // UDP receiver reports `packets` as the max sequence seen
            // (received + lost, matching iperf3), `errors` as the lost count,
            // and `jitter` in seconds.
            let recv = if sender { &remote } else { &local };
            let packets: u64 = recv.streams.iter().map(|s| s.packets).sum();
            let lost: u64 = recv.streams.iter().map(|s| s.errors).sum();
            let jitter_secs = if recv.streams.is_empty() {
                0.0
            } else {
                recv.streams.iter().map(|s| s.jitter).sum::<f64>() / recv.streams.len() as f64
            };
            let lost_percent = if packets > 0 {
                100.0 * lost as f64 / packets as f64
            } else {
                0.0
            };
            Some(UdpStats {
                jitter_secs,
                lost,
                packets,
                lost_percent,
            })
        } else {
            None
        };

        TestResult {
            udp: self.params.udp,
            reverse: self.params.reverse,
            duration_seconds: duration,
            sent_bytes,
            received_bytes,
            send_bits_per_second: bits_per_second(sent_bytes, duration),
            receive_bits_per_second: bits_per_second(received_bytes, duration),
            local,
            remote,
            udp_stats,
            connection: None,
        }
    }

    /// The single teardown path: signal any sender *and* any receiver to
    /// stop, close every stream's channel, then join (or, as a last resort,
    /// abort) every spawned task. Called on every exit from `run_loop` —
    /// success or error alike — by `run_tcp`.
    ///
    /// Signaling `stop_receivers` here (and only here — never from the
    /// duration timer) is what makes `s.close()` below safe to call
    /// unconditionally, even for a reverse-mode stream whose receiver task
    /// may be sitting in a pending read on an idle socket: without it,
    /// `close()`'s `self.channel.lock().await` would queue behind a lock the
    /// receiver never releases, hanging teardown (and therefore
    /// `run_client`) forever — see `streams::runner::run_receiver`'s doc.
    async fn teardown(&mut self) {
        let _ = self.stop_senders.send(true);
        let _ = self.stop_receivers.send(true);
        for s in &mut self.streams {
            s.close().await;
        }
        for s in &mut self.streams {
            if let Some(task) = s.task.take() {
                let abort_handle = task.abort_handle();
                if time::timeout(Duration::from_secs(2), task).await.is_err() {
                    abort_handle.abort();
                }
            }
        }
    }
}

async fn run_tcp(
    host: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    let control = TcpPipe::connect(host, opts.port, CONNECT_TIMEOUT).await?;
    run_control(control, host, opts, on_interval).await
}

#[cfg(feature = "ws")]
async fn run_ws(
    host: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    let control = WsPipe::connect(host, opts.port, WS_CONNECT_TIMEOUT).await?;
    run_control(control, host, opts, on_interval).await
}

/// Builds the session and drives the (transport-agnostic) control loop over
/// `control`, tearing everything down on exit. `run_tcp`/`run_ws` are the thin
/// wrappers that supply the concrete control pipe; the state machine itself is
/// written once, generic over `C: BytePipe`.
async fn run_control<C: BytePipe>(
    mut control: C,
    host: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    let cookie = make_cookie();
    let cookie_bytes = cookie_to_bytes(&cookie);
    let params = build_params(&opts);

    let mut session = Session::new(
        host.to_string(),
        opts.port,
        opts.transport,
        cookie_bytes,
        params,
        opts.interval,
        on_interval,
    );
    let outcome = session.run_loop(&mut control).await;
    session.teardown().await;
    control.close().await;
    outcome
}

/// Derives the wire [`TestParams`] from client options, applying iperf3's
/// UDP-vs-TCP defaults for block length and default pacing.
fn build_params(opts: &ClientOptions) -> TestParams {
    let default_len = if opts.udp {
        DEFAULT_UDP_LEN
    } else {
        DEFAULT_TCP_LEN
    };
    // UDP is paced by default (iperf3's 1 Mbit/s); TCP/WS/iroh is unpaced (0).
    let default_bandwidth = if opts.udp { DEFAULT_UDP_BANDWIDTH } else { 0 };
    TestParams {
        udp: opts.udp,
        time: opts.duration,
        parallel: opts.parallel,
        len: opts.len.unwrap_or(default_len),
        reverse: opts.reverse,
        bandwidth: opts.bandwidth.unwrap_or(default_bandwidth),
    }
}

async fn open_tcp_stream(
    host: &str,
    port: u16,
    cookie: &[u8; COOKIE_SIZE],
) -> Result<Box<dyn crate::streams::channel::DataChannel>> {
    let mut pipe = TcpPipe::connect(host, port, CONNECT_TIMEOUT).await?;
    pipe.write_all(cookie).await?;
    let channel = pipe.into_data_channel()?;
    Ok(Box::new(channel))
}

#[cfg(feature = "ws")]
async fn open_ws_stream(
    host: &str,
    port: u16,
    cookie: &[u8; COOKIE_SIZE],
) -> Result<Box<dyn crate::streams::channel::DataChannel>> {
    let mut pipe = WsPipe::connect(host, port, WS_CONNECT_TIMEOUT).await?;
    pipe.write_all(cookie).await?;
    let channel = pipe.into_data_channel()?;
    Ok(Box::new(channel))
}

/// Fixed-address native QUIC sibling of the TCP/WS/iroh client paths.
#[cfg(feature = "quic")]
async fn run_quic(
    host: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    use crate::error::SetupPhase;
    use crate::transport::quic::channel::QuicPipe;
    use crate::transport::quic::endpoint::{
        CLOSE_TIMEOUT as QUIC_CLOSE_TIMEOUT, CONNECT_TIMEOUT as QUIC_CONNECT_TIMEOUT, QuicEndpoint,
    };

    let setup_error = |phase, detail: String| NetsuError::Setup {
        transport: "quic",
        phase,
        detail,
    };
    let trust = opts
        .quic
        .as_ref()
        .ok_or_else(|| NetsuError::Protocol("missing QUIC client options".into()))?;
    let verification = if trust.insecure { "insecure" } else { "ca" };
    let client_config = crate::transport::quic::tls::client_config(trust)?;

    let addresses = tokio::time::timeout(
        QUIC_CONNECT_TIMEOUT,
        tokio::net::lookup_host((host, opts.port)),
    )
    .await
    .map_err(|_| {
        setup_error(
            SetupPhase::Resolve,
            format!("timed out after {} seconds", QUIC_CONNECT_TIMEOUT.as_secs()),
        )
    })?
    .map_err(|error| setup_error(SetupPhase::Resolve, error.to_string()))?
    .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(setup_error(
            SetupPhase::Resolve,
            "host resolved to no addresses".into(),
        ));
    }

    let endpoint = QuicEndpoint::bind_client(client_config)?;
    let deadline = tokio::time::Instant::now() + QUIC_CONNECT_TIMEOUT;
    let mut last_error = None;
    let mut connected = None;
    for address in addresses {
        match tokio::time::timeout_at(deadline, endpoint.connect(address, host)).await {
            Ok(Ok(value)) => {
                connected = Some(value);
                break;
            }
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(setup_error(
                    SetupPhase::QuicHandshake,
                    format!(
                        "overall connect deadline exceeded after {} seconds",
                        QUIC_CONNECT_TIMEOUT.as_secs()
                    ),
                ));
                break;
            }
        }
    }
    let (connection, handshake) = match connected {
        Some(value) => value,
        None => {
            // No connection exists to drain. Send endpoint close, but reserve
            // most of the documented 12-second outer bound for the 10-second
            // handshake itself rather than waiting the full shutdown ceiling.
            let _ = tokio::time::timeout(QUIC_CLOSE_TIMEOUT / 2, endpoint.close()).await;
            return Err(last_error.unwrap_or_else(|| {
                setup_error(SetupPhase::QuicHandshake, "no address connected".into())
            }));
        }
    };

    let control_stream = tokio::time::timeout(QUIC_STREAMS_TIMEOUT, connection.open_bi()).await;
    let (send, recv) = match control_stream {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            connection.close(0u32.into(), b"control stream failed");
            endpoint.close().await;
            return Err(setup_error(SetupPhase::ChannelsOpen, error.to_string()));
        }
        Err(_) => {
            connection.close(0u32.into(), b"control stream timeout");
            endpoint.close().await;
            return Err(setup_error(
                SetupPhase::ChannelsOpen,
                format!(
                    "control stream timed out after {} seconds",
                    QUIC_STREAMS_TIMEOUT.as_secs()
                ),
            ));
        }
    };
    let mut control = QuicPipe::new(send, recv);

    let cookie = make_cookie();
    let cookie_bytes = cookie_to_bytes(&cookie);
    let params = build_params(&opts);
    let mut session = Session::new(
        String::new(),
        0,
        opts.transport,
        cookie_bytes,
        params,
        opts.interval,
        on_interval,
    );
    session.quic_connection = Some(connection.clone());

    let outcome = session.run_loop(&mut control).await;
    session.teardown().await;
    control.close().await;
    let info = crate::transport::quic::observe::observe(&connection, handshake, verification);
    connection.close(0u32.into(), b"test done");
    endpoint.close().await;

    let mut result = outcome?;
    result.connection = Some(ConnectionInfo::Quic(info));
    Ok(result)
}

#[cfg(feature = "quic")]
async fn open_quic_stream(
    connection: &quinn::Connection,
    cookie: &[u8; COOKIE_SIZE],
) -> Result<Box<dyn crate::streams::channel::DataChannel>> {
    use crate::error::SetupPhase;
    use crate::transport::quic::channel::QuicChannel;

    let (mut send, receive) = connection
        .open_bi()
        .await
        .map_err(|error| NetsuError::Setup {
            transport: "quic",
            phase: SetupPhase::ChannelsOpen,
            detail: error.to_string(),
        })?;
    send.write_all(cookie)
        .await
        .map_err(|error| NetsuError::Setup {
            transport: "quic",
            phase: SetupPhase::ChannelsOpen,
            detail: format!("failed to write data-stream cookie: {error}"),
        })?;
    Ok(Box::new(QuicChannel::new(send, receive)))
}

/// The iroh sibling of [`run_tcp`]/[`run_ws`]: dial `peer` (an `EndpointTicket`
/// string) over one iroh connection, open the control bi-stream, and drive the
/// same [`Session`] state machine. The connection is attached to the session so
/// its data streams open as bi-streams on it (see [`open_iroh_stream`]).
#[cfg(feature = "iroh")]
async fn run_iroh(
    peer: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    use crate::p2p::{THROUGHPUT_ALPN, endpoint};
    use crate::transport::iroh::IrohPipe;

    let iroh_err = |e: anyhow::Error| NetsuError::Protocol(format!("{e:#}"));

    let addr = endpoint::parse_ticket(peer).map_err(iroh_err)?;
    let ep = endpoint::bind_client(opts.direct_only, true)
        .await
        .map_err(iroh_err)?;
    let connection = tokio::time::timeout(
        CONTROL_TIMEOUT,
        endpoint::connect(&ep, addr, THROUGHPUT_ALPN),
    )
    .await
    .map_err(|_| NetsuError::Timeout)?
    .map_err(iroh_err)?;

    // The control stream is the first bi-stream; the server classifies it by the
    // cookie the session writes first, exactly like a TCP control connection.
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|e| NetsuError::Protocol(format!("open iroh control stream: {e}")))?;
    let mut control = IrohPipe::new(send, recv);

    let cookie = make_cookie();
    let cookie_bytes = cookie_to_bytes(&cookie);
    let params = build_params(&opts);
    let mut session = Session::new(
        String::new(), // host/port are unused for iroh data streams
        0,
        opts.transport,
        cookie_bytes,
        params,
        opts.interval,
        on_interval,
    );
    session.iroh_connection = Some(connection.clone());

    let outcome = session.run_loop(&mut control).await;
    session.teardown().await;
    control.close().await;

    // Snapshot the path (direct/relay + RTT) before tearing the connection down.
    let info = crate::p2p::observe::observe(&connection);
    connection.close(0u32.into(), b"test done");
    ep.close().await;

    let mut result = outcome?;
    if opts.direct_only && info.observed_path == "relay" {
        return Err(NetsuError::Protocol(
            "direct-only: the connection used a relay path".into(),
        ));
    }
    result.connection = Some(ConnectionInfo::Iroh(info));
    Ok(result)
}

/// Opens one iroh data stream: a fresh bi-stream on the shared connection whose
/// first bytes are the session cookie (the server matches it to the active
/// session's CREATE_STREAMS window, same as a TCP data connection).
#[cfg(feature = "iroh")]
async fn open_iroh_stream(
    connection: &iroh::endpoint::Connection,
    cookie: &[u8; COOKIE_SIZE],
) -> Result<Box<dyn crate::streams::channel::DataChannel>> {
    use crate::transport::iroh::IrohChannel;
    let (mut send, recv) = connection
        .open_bi()
        .await
        .map_err(|e| NetsuError::Protocol(format!("open iroh data stream: {e}")))?;
    send.write_all(cookie)
        .await
        .map_err(|e| NetsuError::Protocol(format!("write iroh data cookie: {e}")))?;
    Ok(Box::new(IrohChannel::new(send, recv)))
}

/// Polls the duration-timer `Sleep` if armed, else never resolves — lets the
/// duration-timer `select!` arm be conditionally present without needing an
/// `Option`-shaped branch syntax tokio doesn't have.
async fn fire_if_armed(sleep: &mut Option<Pin<Box<Sleep>>>) {
    match sleep {
        Some(s) => s.as_mut().await,
        None => std::future::pending::<()>().await,
    }
}

/// Same trick as [`fire_if_armed`], for the interval-reporting ticker.
async fn tick_if_armed(ticker: &mut Option<Interval>) {
    match ticker {
        Some(t) => {
            t.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}
