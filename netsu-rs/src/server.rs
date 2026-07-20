//! Server control state machine: accepts connections, runs one test at a time
//! against a real iperf3 (or netsu) client, and mirrors the client's lifecycle
//! from the other side. Ported from `packages/netsu/src/server.ts`; see
//! `PROTOCOL.md`'s connection-acceptance rule and "Error behavior".
//!
//! ## Why [`ServerCore::handle_connection`] is generic from the start
//!
//! Unlike the client (see `client.rs`'s module doc), the server's connection
//! handler *is* the state machine, so writing a concrete `handle_connection`
//! plus a sibling `handle_connection_ws` for Task 9 would duplicate the whole
//! machine. Instead `handle_connection<P: BytePipe>` is generic now: each
//! accept loop (TCP here, WS in Task 9) monomorphizes it with its own concrete
//! pipe type. `BytePipe` is not dyn-compatible (`Box<dyn BytePipe>` does not
//! compile — native async-fn-in-trait), so generics, not trait objects, are
//! the mechanism; a single connection's pipe type is always known at its
//! accept site, so no heterogeneous storage is needed. The transport-specific
//! step — turning a data-stream pipe into a [`DataChannel`] — is passed in as
//! a `to_channel` closure, exactly as `server.ts` passes `() => new
//! TcpDataChannel(pipe.detach())`.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time;

use crate::client::Transport;
use crate::error::{NetsuError, Result};
use crate::protocol::framing::{MAX_JSON, read_json, read_state, write_json, write_state};
use crate::protocol::params::{self, TestParams};
use crate::protocol::pipe::BytePipe;
use crate::protocol::results::{self, EndResults};
use crate::protocol::states::{
    ACCESS_DENIED, COOKIE_SIZE, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS, PARAM_EXCHANGE,
    SERVER_ERROR, TEST_END, TEST_RUNNING, TEST_START,
};
use crate::stats::IntervalMeter;
use crate::streams::channel::DataChannel;
use crate::streams::runner::{
    SharedChannel, SharedCounters, SharedMeter, StreamCounters, next_stream_id, run_receiver,
    run_sender,
};
use crate::transport::tcp::TcpPipe;

/// Control-channel timeout for any expected read outside `TEST_RUNNING`
/// (30s, matches `PROTOCOL.md`'s "Control-channel timeouts").
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
/// `PROTOCOL.md`: during `TEST_RUNNING` the server caps the test at
/// `time + 10s` as a safety net (the client owns the real duration timer).
const TEST_RUNNING_GRACE: Duration = Duration::from_secs(10);
const DEFAULT_MAX_TEST_SECONDS: u32 = 3600;

/// Server-side configuration.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub port: u16,
    pub transport: Transport,
    /// Upper bound on a client-requested `time` (PARAM_EXCHANGE), in seconds.
    /// `protocol::params`'s own wire bound (86400s) is a JSON-payload sanity
    /// ceiling, not an operational one — the server waits `time + 10s` for
    /// TEST_END while holding the single-test lock, so an unauthenticated peer
    /// sending `{"time": 86400}` would otherwise deny service for a full day.
    /// Default 3600s (1h) is generous but bounded.
    pub max_test_seconds: u32,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            port: 5201,
            transport: Transport::Tcp,
            max_test_seconds: DEFAULT_MAX_TEST_SECONDS,
        }
    }
}

/// A running server. Dropping it does not stop the server — call
/// [`NetsuServer::close`], which stops accepting, aborts any active test, and
/// releases the port.
pub struct NetsuServer {
    pub port: u16,
    shutdown: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl NetsuServer {
    /// Stops accepting new connections, aborts any in-progress test, and waits
    /// for the accept loop to wind down. Does not hang on a connection still
    /// sitting in its cookie read: those handler tasks are detached and the
    /// listener is dropped here, freeing the port immediately.
    pub async fn close(self) {
        let _ = self.shutdown.send(true);
        // The accept loop aborts the active session on its way out; awaiting it
        // is bounded (the abort signal is immediate, the loop then returns).
        let _ = self.accept_task.await;
    }
}

/// Binds `opts.port` and starts accepting. Port `0` binds an ephemeral port,
/// discoverable via the returned [`NetsuServer::port`].
pub async fn start_server(opts: ServerOptions) -> Result<NetsuServer> {
    if opts.transport != Transport::Tcp {
        // Task 9 replaces this line with a WebSocket accept loop that reuses
        // the same `ServerCore::handle_connection`.
        return Err(NetsuError::Protocol(
            "ws server wired in a later task".into(),
        ));
    }

    let listener = TcpListener::bind(("127.0.0.1", opts.port)).await?;
    let port = listener.local_addr()?.port();
    let core = Arc::new(ServerCore::new(opts.max_test_seconds));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                accepted = listener.accept() => {
                    let stream = match accepted {
                        Ok((stream, _)) => stream,
                        // A transient accept error (e.g. a peer that reset
                        // before the handshake) must not kill the loop.
                        Err(_) => continue,
                    };
                    let pipe = TcpPipe::from_stream(stream);
                    let core = core.clone();
                    // Detached per-connection task: `close()` does not await
                    // these, so a connection stuck in its 30s cookie read can
                    // never make `close()` hang. Dropping the listener above
                    // is what actually frees the port.
                    tokio::spawn(async move {
                        core.handle_connection(pipe, |p: TcpPipe| {
                            p.into_data_channel()
                                .map(|c| Box::new(c) as Box<dyn DataChannel>)
                        })
                        .await;
                    });
                }
            }
        }
        // Stop accepting first (listener drops when this scope ends), then
        // abort whatever test is running so its control loop returns and its
        // lock slot is released.
        core.abort();
    });

    Ok(NetsuServer {
        port,
        shutdown: shutdown_tx,
        accept_task,
    })
}

/// The single-test lock plus the accept rule. One [`ServerCore`] per listening
/// port; at most one [`ServerSession`] is active at a time.
struct ServerCore {
    max_test_seconds: u32,
    // A `std::sync::Mutex`, not tokio's: the critical section (classify a
    // connection / install or clear the active handle) never awaits while
    // holding the lock, so an async mutex buys nothing — and a sync lock is
    // what lets [`ActiveSlotGuard`] clear the slot from a `Drop` impl (which
    // cannot be async), making the single-test lock release panic-safe.
    active: StdMutex<Option<ActiveHandle>>,
}

/// Clears `ServerCore::active` on drop — including a panic-unwind out of a
/// running session. Installed right after a session becomes active, so the slot
/// is released on *every* exit path (normal, error, or panic), the panic-safe
/// equivalent of `server.ts`'s `finally { this.#active = null }`. A plain
/// trailing `*active = None` would be skipped on unwind, leaving the slot
/// `Some` forever — the "accepts one test then refuses forever" failure.
struct ActiveSlotGuard<'a>(&'a StdMutex<Option<ActiveHandle>>);

impl Drop for ActiveSlotGuard<'_> {
    fn drop(&mut self) {
        // `into_inner` on a poisoned lock: recover rather than panic-in-drop
        // (which would abort). The slot must be cleared regardless.
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// What [`ServerCore::handle_connection`] needs to route a data-stream
/// connection to the currently-running session, and to abort it on `close()`.
struct ActiveHandle {
    cookie: [u8; COOKIE_SIZE],
    shared: Arc<SessionShared>,
}

/// The slice of a running session's state that connection-accept tasks touch
/// concurrently with the session's own control loop.
struct SessionShared {
    /// True only during the CREATE_STREAMS window, when data-stream
    /// connections bearing the session cookie should be accepted as streams.
    awaiting: AtomicBool,
    /// Delivers a newly-accepted data channel to the running session.
    stream_tx: mpsc::UnboundedSender<Box<dyn DataChannel>>,
    /// Fired by `close()`/abort to make the session's control loop return.
    abort: watch::Sender<bool>,
}

/// A connection's classification, decided under the lock and acted on after
/// releasing it. Carrying the pieces out of the lock keeps the (possibly slow)
/// stream conversion and the session run off the critical section.
enum Decision {
    /// A data stream for the active session: deliver a channel over this sender.
    Stream(Arc<SessionShared>),
    /// A test is active and this is not one of its streams → ACCESS_DENIED.
    Busy,
    /// A fresh control connection; the handle is already installed as active.
    Run(RunParts),
}

/// The session-side halves of the channels whose sender-side halves went into
/// the installed [`SessionShared`].
struct RunParts {
    shared: Arc<SessionShared>,
    stream_rx: mpsc::UnboundedReceiver<Box<dyn DataChannel>>,
    abort_rx: watch::Receiver<bool>,
}

impl ServerCore {
    fn new(max_test_seconds: u32) -> Self {
        ServerCore {
            max_test_seconds,
            active: StdMutex::new(None),
        }
    }

    /// The accept rule (`PROTOCOL.md`): read the 37-byte cookie; if no test is
    /// active this is a new control connection; if a test is active and the
    /// cookie matches during its CREATE_STREAMS window it is a data stream;
    /// otherwise reply ACCESS_DENIED and close.
    async fn handle_connection<P, F>(&self, mut pipe: P, to_channel: F)
    where
        P: BytePipe,
        F: FnOnce(P) -> Result<Box<dyn DataChannel>>,
    {
        let cookie = match pipe.read_exact(COOKIE_SIZE, Some(CONTROL_TIMEOUT)).await {
            Ok(bytes) => {
                let mut c = [0u8; COOKIE_SIZE];
                c.copy_from_slice(&bytes);
                c
            }
            Err(_) => {
                pipe.close().await;
                return;
            }
        };

        // Classify and, for a new control connection, atomically install the
        // active handle — all under one lock acquisition, so two simultaneous
        // fresh connections can't both become active (the TS version relies on
        // the single-threaded event loop for the same guarantee).
        let decision = {
            let mut guard = self.active.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(h) if h.cookie == cookie && h.shared.awaiting.load(Ordering::Acquire) => {
                    Decision::Stream(h.shared.clone())
                }
                Some(_) => Decision::Busy,
                None => {
                    let (stream_tx, stream_rx) = mpsc::unbounded_channel();
                    let (abort, abort_rx) = watch::channel(false);
                    let shared = Arc::new(SessionShared {
                        awaiting: AtomicBool::new(false),
                        stream_tx,
                        abort,
                    });
                    *guard = Some(ActiveHandle {
                        cookie,
                        shared: shared.clone(),
                    });
                    Decision::Run(RunParts {
                        shared,
                        stream_rx,
                        abort_rx,
                    })
                }
            }
        };

        match decision {
            Decision::Stream(shared) => match to_channel(pipe) {
                Ok(channel) => {
                    // If the session already stopped awaiting (raced past
                    // CREATE_STREAMS), the receiver is gone and this send
                    // errors — harmless, the stream is simply dropped.
                    let _ = shared.stream_tx.send(channel);
                }
                Err(e) => {
                    eprintln!("netsu server: data stream conversion failed: {e}");
                }
            },
            Decision::Busy => {
                let _ = write_state(&mut pipe, ACCESS_DENIED).await;
                pipe.close().await;
            }
            Decision::Run(parts) => {
                // Clears the active slot on every exit below, panic included —
                // see `ActiveSlotGuard`. Held until this arm returns.
                let _slot = ActiveSlotGuard(&self.active);
                let mut session = ServerSession::new(
                    parts.shared,
                    parts.stream_rx,
                    parts.abort_rx,
                    self.max_test_seconds,
                );
                let outcome = session.run(&mut pipe).await;
                if let Err(err) = &outcome {
                    // Do not swallow the reason: the peer only ever sees
                    // SERVER_ERROR on the wire (it carries none), so without
                    // this an operator sees nothing. Best-effort SERVER_ERROR,
                    // then close.
                    eprintln!("netsu server: session failed: {err}");
                    let _ = write_state(&mut pipe, SERVER_ERROR).await;
                }
                session.teardown().await;
                pipe.close().await;
            }
        }
    }

    fn abort(&self) {
        if let Some(h) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = h.shared.abort.send(true);
        }
    }
}

/// One data stream's bookkeeping on the server side — the mirror of the
/// client's `StreamState`.
struct ServerStream {
    counters: SharedCounters,
    channel: SharedChannel,
    task: Option<JoinHandle<()>>,
    closed: bool,
    /// Snapshot of `channel.error()` taken at close time, before we tear the
    /// channel down ourselves — see `client.rs`'s `StreamState::latched_error`
    /// for the full rationale (teardown noise must not be reported as a
    /// genuine transfer failure).
    latched_error: Option<String>,
}

impl ServerStream {
    async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let mut ch = self.channel.lock().await;
        self.latched_error = ch.error().map(|e| e.to_string());
        ch.close().await;
    }
}

/// All mutable state for one server-side test session.
struct ServerSession {
    shared: Arc<SessionShared>,
    stream_rx: mpsc::UnboundedReceiver<Box<dyn DataChannel>>,
    abort_rx: watch::Receiver<bool>,
    max_test_seconds: u32,

    streams: Vec<ServerStream>,
    meter: SharedMeter,
    // Forward mode: the server receives. Reverse mode: the server sends. Two
    // shutdown signals, mirroring the client: receivers must only stop at
    // teardown (never earlier, or a still-writing peer is RST mid-write),
    // while senders stop as soon as TEST_END is observed.
    stop_senders: watch::Sender<bool>,
    stop_senders_rx: watch::Receiver<bool>,
    stop_receivers: watch::Sender<bool>,
    stop_receivers_rx: watch::Receiver<bool>,

    running: bool,
    start_instant: Option<Instant>,
    end_instant: Option<Instant>,
}

impl ServerSession {
    fn new(
        shared: Arc<SessionShared>,
        stream_rx: mpsc::UnboundedReceiver<Box<dyn DataChannel>>,
        abort_rx: watch::Receiver<bool>,
        max_test_seconds: u32,
    ) -> Self {
        let (stop_senders, stop_senders_rx) = watch::channel(false);
        let (stop_receivers, stop_receivers_rx) = watch::channel(false);
        ServerSession {
            shared,
            stream_rx,
            abort_rx,
            max_test_seconds,
            streams: Vec::new(),
            meter: Arc::new(Mutex::new(IntervalMeter::new(Instant::now()))),
            stop_senders,
            stop_senders_rx,
            stop_receivers,
            stop_receivers_rx,
            running: false,
            start_instant: None,
            end_instant: None,
        }
    }

    /// The server-side lifecycle, the mirror of `client.rs`'s `run_loop`. On
    /// any error the caller (`handle_connection`) sends SERVER_ERROR and runs
    /// [`ServerSession::teardown`].
    async fn run<P: BytePipe>(&mut self, pipe: &mut P) -> Result<()> {
        write_state(pipe, PARAM_EXCHANGE).await?;
        let params_json: serde_json::Value =
            read_json(pipe, MAX_JSON, Some(CONTROL_TIMEOUT)).await?;
        let params = params::decode(params_json)?;
        if params.time > self.max_test_seconds {
            return Err(NetsuError::Protocol(format!(
                "requested time {}s exceeds this server's max of {}s",
                params.time, self.max_test_seconds
            )));
        }
        if params.udp {
            // Task 8 replaces this line with the UDP data-plane setup.
            return Err(NetsuError::Protocol("udp wired in a later task".into()));
        }

        // Open the CREATE_STREAMS window before announcing it, so a data-stream
        // connection that races in the instant after the client sees
        // CREATE_STREAMS is recognized rather than rejected as a stray.
        self.shared.awaiting.store(true, Ordering::Release);
        write_state(pipe, CREATE_STREAMS).await?;
        self.collect_streams(params.parallel, &params).await?;
        self.shared.awaiting.store(false, Ordering::Release);

        write_state(pipe, TEST_START).await?;
        self.running = true;
        self.start_instant = Some(Instant::now());
        write_state(pipe, TEST_RUNNING).await?;
        self.start_running(&params);

        // Safety cap: the client owns the real duration timer; the server just
        // waits for TEST_END with a +10s grace, aborting if `close()` fires.
        let wait = Duration::from_secs(params.time as u64) + TEST_RUNNING_GRACE;
        let state = tokio::select! {
            biased;
            _ = self.abort_rx.changed() => return Err(NetsuError::Protocol("aborted".into())),
            st = read_state(pipe, Some(wait)) => st?,
        };
        self.running = false;
        self.end_instant = Some(Instant::now());
        if state != TEST_END {
            return Err(NetsuError::Protocol(format!(
                "expected TEST_END, got {state}"
            )));
        }

        // Stop senders (reverse mode), then close every stream. Closing after
        // observing TEST_END means a latched error reflects a genuine
        // mid-transfer problem, not teardown-timing noise (see `server.ts`).
        // Receivers (forward mode) must be signaled before the close() below
        // can acquire their channel lock — they hold it across a pending read.
        let _ = self.stop_senders.send(true);
        let _ = self.stop_receivers.send(true);
        for s in &mut self.streams {
            s.close().await;
        }
        for s in &self.streams {
            if let Some(err) = s.latched_error.as_ref() {
                // TCP only in this task: a write failure is a genuine transfer
                // failure. (UDP's count-and-continue policy lands in Task 8.)
                return Err(NetsuError::Protocol(format!("data stream failed: {err}")));
            }
        }

        write_state(pipe, EXCHANGE_RESULTS).await?;
        // Read the client's view first (server sends its own after), then
        // discard it — the client's TestResult is built client-side.
        let _client_json: serde_json::Value =
            read_json(pipe, MAX_JSON, Some(CONTROL_TIMEOUT)).await?;
        let local = self.local_results(&params).await;
        write_json(pipe, &results::encode(&local)).await?;
        write_state(pipe, DISPLAY_RESULTS).await?;
        let _ = read_state(pipe, Some(CONTROL_TIMEOUT)).await?; // IPERF_DONE
        Ok(())
    }

    /// Waits for `n` data streams to arrive over `stream_rx`, adding each.
    /// Aborts on `close()` or if a stream fails to arrive within the timeout.
    async fn collect_streams(&mut self, n: u32, params: &TestParams) -> Result<()> {
        for _ in 0..n {
            let channel = tokio::select! {
                biased;
                _ = self.abort_rx.changed() => return Err(NetsuError::Protocol("aborted".into())),
                got = time::timeout(CONTROL_TIMEOUT, self.stream_rx.recv()) => {
                    match got {
                        Ok(Some(ch)) => ch,
                        // Sender dropped (should not happen while we hold the
                        // active slot) or timed out waiting for a stream.
                        Ok(None) | Err(_) => {
                            return Err(NetsuError::Protocol(
                                "timed out waiting for data streams".into(),
                            ));
                        }
                    }
                }
            };
            self.add_stream(channel, params);
        }
        Ok(())
    }

    /// Adds one data stream, assigning the next iperf3-quirky id. In forward
    /// mode (server receives) the receiver task is attached immediately, at
    /// stream-arrival time, mirroring the client's `open_stream`; reverse-mode
    /// senders instead wait for [`ServerSession::start_running`].
    fn add_stream(&mut self, channel: Box<dyn DataChannel>, params: &TestParams) {
        let id = next_stream_id(self.streams.len());
        let counters: SharedCounters = Arc::new(Mutex::new(StreamCounters::new(id)));
        let channel: SharedChannel = Arc::new(Mutex::new(channel));
        let task = if !params.reverse {
            Some(tokio::spawn(run_receiver(
                channel.clone(),
                counters.clone(),
                self.meter.clone(),
                self.stop_receivers_rx.clone(),
            )))
        } else {
            None
        };
        self.streams.push(ServerStream {
            counters,
            channel,
            task,
            closed: false,
            latched_error: None,
        });
    }

    /// Reverse mode only: start a sender task per stream. Forward-mode
    /// receivers were already attached in [`ServerSession::add_stream`].
    fn start_running(&mut self, params: &TestParams) {
        if !params.reverse {
            return;
        }
        let len = params.len;
        for s in &mut self.streams {
            s.task = Some(tokio::spawn(run_sender(
                s.channel.clone(),
                s.counters.clone(),
                self.meter.clone(),
                len,
                self.stop_senders_rx.clone(),
            )));
        }
    }

    async fn local_results(&self, params: &TestParams) -> EndResults {
        let sender = params.reverse; // server sends when the test is reversed
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
                retransmits: -1,
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

    /// The single teardown path: signal both sender and receiver shutdown,
    /// close every channel, then join (or, as a last resort, abort) every
    /// spawned task. Signaling `stop_receivers` here is what makes `close()`
    /// safe even for a receiver sitting in a pending read on an idle socket —
    /// see `streams::runner::run_receiver`'s doc.
    async fn teardown(&mut self) {
        self.running = false;
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
