//! Client control state machine: drives one test session against a real
//! iperf3 (or netsu) server over the TCP control channel, per `PROTOCOL.md`'s
//! "Test lifecycle". Ported from `packages/netsu/src/client.ts` — see that
//! file's comments for the TEST_END ordering, the reverse-mode teardown
//! race, and the EXCHANGE_RESULTS idempotence fix this module also honors.
//!
//! ## Transport dispatch: generics/monomorphization, not an enum wrapper
//!
//! `protocol::pipe::BytePipe` uses native async-fn-in-trait and is therefore
//! **not** dyn-compatible (`Box<dyn BytePipe>` does not compile — confirmed
//! against this repo's pinned rustc). Only one control-channel transport
//! (`TcpPipe`) exists in this task, so there is exactly one call site and no
//! polymorphism to resolve yet. When Task 9 adds a WS control channel, the
//! natural extension is to add a sibling concrete function (e.g. `run_ws`)
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
use crate::protocol::params::{self, DEFAULT_TCP_LEN, TestParams};
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
use crate::transport::tcp::{CONNECT_TIMEOUT, TcpPipe};

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
    Ws,
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
        }
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
}

/// Runs one client test session against `host`, tearing down every socket
/// and task it opened before returning — on success or on error alike.
///
/// UDP and WS are deliberately not implemented in this task: Tasks 8 and 9
/// replace exactly the two lines below that construct these errors.
pub async fn run_client(
    host: &str,
    opts: ClientOptions,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
) -> Result<TestResult> {
    if opts.udp {
        return Err(NetsuError::Protocol("udp wired in a later task".into()));
    }
    match opts.transport {
        Transport::Ws => Err(NetsuError::Protocol("ws wired in a later task".into())),
        Transport::Tcp => run_tcp(host, opts, on_interval).await,
    }
}

/// One data stream's bookkeeping: the shared channel/counters the spawned
/// sender-or-receiver task also holds, plus that task's handle so teardown
/// can join (or, failing that, abort) it.
struct StreamState {
    counters: SharedCounters,
    channel: SharedChannel,
    task: Option<JoinHandle<()>>,
    /// Snapshot of `channel.error()` taken at the moment we force-closed this
    /// stream (forward mode, duration timer), *before* calling `close()`.
    /// Mirrors `client.ts`'s `TcpDataChannel` doc: forcibly shutting down a
    /// socket while a sender task races it for the same lock can itself
    /// induce a fresh, self-inflicted write failure (e.g. a doomed write
    /// that slips in right after shutdown) — that teardown noise must not
    /// be reported as a genuine transfer failure. Reading `channel.error()`
    /// live from `handle_exchange_results`, after close() already ran, would
    /// pick up exactly that noise; consulting this pre-close snapshot
    /// instead (when present) avoids it. `None` until this stream has been
    /// force-closed.
    latched_error: Option<String>,
}

impl StreamState {
    /// Snapshots any error already latched on the channel, then closes it.
    /// Idempotent: closing twice (duration timer, then final teardown) is
    /// safe — the snapshot is only taken once, and closing an
    /// already-closed socket is a harmless no-op.
    async fn close(&mut self) {
        let mut ch = self.channel.lock().await;
        if self.latched_error.is_none() {
            self.latched_error = ch.error().map(|e| e.to_string());
        }
        ch.close().await;
    }
}

/// All mutable state for one client test session. Lives only inside
/// [`run_tcp`]; not part of the public interface.
struct Session {
    host: String,
    port: u16,
    cookie: [u8; COOKIE_SIZE],
    params: TestParams,
    interval: Option<Duration>,
    on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,

    streams: Vec<StreamState>,
    meter: SharedMeter,
    /// Shared shutdown signal for forward-mode sender tasks only. Receiver
    /// tasks (reverse mode) never observe this — see protocol fact 3 in the
    /// module-level docs and `streams::runner::run_receiver`.
    stop_senders: watch::Sender<bool>,
    stop_senders_rx: watch::Receiver<bool>,

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
        cookie: [u8; COOKIE_SIZE],
        params: TestParams,
        interval: Option<Duration>,
        on_interval: Option<Box<dyn FnMut(IntervalReport) + Send>>,
    ) -> Self {
        let (stop_senders, stop_senders_rx) = watch::channel(false);
        Session {
            host,
            port,
            cookie,
            params,
            interval,
            on_interval,
            streams: Vec::new(),
            meter: Arc::new(Mutex::new(crate::stats::IntervalMeter::new(Instant::now()))),
            stop_senders,
            stop_senders_rx,
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
    async fn run_loop(&mut self, control: &mut TcpPipe) -> Result<TestResult> {
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
                            for _ in 0..self.params.parallel {
                                self.open_stream().await?;
                            }
                        }
                        TEST_RUNNING => self.start_running(),
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

    /// Opens one TCP data stream, assigns it the next iperf3-quirky id, and
    /// — for reverse mode only — attaches its receiver task immediately
    /// (matching `client.ts`'s `#openTcpStream`, which attaches receivers at
    /// stream-open time; forward-mode senders instead wait for
    /// `start_running`, which fires on `TEST_RUNNING`).
    async fn open_stream(&mut self) -> Result<()> {
        let id = next_stream_id(self.streams.len());
        let channel = open_tcp_stream(&self.host, self.port, &self.cookie).await?;
        let counters: SharedCounters = Arc::new(Mutex::new(StreamCounters::new(id)));
        let channel: SharedChannel = Arc::new(Mutex::new(channel));
        let task = if self.params.reverse {
            Some(tokio::spawn(run_receiver(
                channel.clone(),
                counters.clone(),
                self.meter.clone(),
            )))
        } else {
            None
        };
        self.streams.push(StreamState {
            counters,
            channel,
            task,
            latched_error: None,
        });
        Ok(())
    }

    fn start_running(&mut self) {
        self.running = true;
        let now = Instant::now();
        self.start_instant = Some(now);
        self.meter = Arc::new(Mutex::new(crate::stats::IntervalMeter::new(now)));
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
            let meter = self.meter.clone();
            for s in &mut self.streams {
                let rx = self.stop_senders_rx.clone();
                s.task = Some(tokio::spawn(run_sender(
                    s.channel.clone(),
                    s.counters.clone(),
                    meter.clone(),
                    len,
                    rx,
                )));
            }
        }
    }

    async fn handle_exchange_results(&mut self, control: &mut TcpPipe) -> Result<()> {
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
            // A stream already force-closed (forward mode, duration timer
            // already fired) is checked via its pre-close snapshot, not a
            // live read — see `StreamState::latched_error`'s doc for why a
            // live read here could misreport teardown-induced noise as a
            // genuine transfer failure. A stream not yet closed (reverse
            // mode, or an early EXCHANGE_RESULTS racing ahead of our own
            // duration timer) has no such noise, so a live read is safe.
            let live_err = if s.latched_error.is_some() {
                None
            } else {
                let ch = s.channel.lock().await;
                ch.error().map(|e| e.to_string())
            };
            if let Some(err) = s.latched_error.as_ref().or(live_err.as_ref()) {
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

    async fn local_results(&self) -> EndResults {
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
            udp_stats: None, // UDP not implemented in this task
        }
    }

    /// The single teardown path: signal any sender to stop, close every
    /// stream's channel, then join (or, as a last resort, abort) every
    /// spawned task. Called on every exit from `run_loop` — success or
    /// error alike — by `run_tcp`.
    async fn teardown(&mut self) {
        let _ = self.stop_senders.send(true);
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
    let cookie = make_cookie();
    let cookie_bytes = cookie_to_bytes(&cookie);
    let params = TestParams {
        udp: false,
        time: opts.duration,
        parallel: opts.parallel,
        len: opts.len.unwrap_or(DEFAULT_TCP_LEN),
        reverse: opts.reverse,
        bandwidth: opts.bandwidth.unwrap_or(0),
    };

    let mut control = TcpPipe::connect(host, opts.port, CONNECT_TIMEOUT).await?;
    let mut session = Session::new(
        host.to_string(),
        opts.port,
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
