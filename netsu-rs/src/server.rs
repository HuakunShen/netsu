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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
use crate::stats::{IntervalMeter, IntervalReport};
use crate::streams::channel::DataChannel;
use crate::streams::runner::{
    SharedChannel, SharedCounters, SharedMeter, StreamCounters, next_stream_id, run_receiver,
    run_sender,
};
use crate::transport::tcp::TcpPipe;
use crate::transport::udp::{
    UDP_HEADER_SIZE, probe_max_udp_send_len, run_udp_receiver, run_udp_sender, udp_server_accept,
    udp_server_bind, udp_server_send_reply,
};
#[cfg(feature = "ws")]
use crate::transport::ws::WsPipe;
#[cfg(feature = "ws")]
use tokio::net::TcpStream;
use tokio::net::UdpSocket;

/// Control-channel timeout for any expected read outside `TEST_RUNNING`
/// (30s, matches `PROTOCOL.md`'s "Control-channel timeouts").
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
/// `PROTOCOL.md`: during `TEST_RUNNING` the server caps the test at
/// `time + 10s` as a safety net (the client owns the real duration timer).
const TEST_RUNNING_GRACE: Duration = Duration::from_secs(10);
const DEFAULT_MAX_TEST_SECONDS: u32 = 3600;

/// A live event from a running server test, for CLI display (mirrors iperf3's
/// server-side log). The library emits nothing unless `ServerOptions.on_event`
/// is set.
pub enum ServerEvent {
    /// One reporting interval of throughput through the server's data path.
    Interval(IntervalReport),
    /// The test finished: total bytes and average throughput over its duration.
    Complete {
        duration_seconds: f64,
        bytes: u64,
        bits_per_second: f64,
    },
}

/// A shared, `Fn` sink for [`ServerEvent`]s — shared because one server serves
/// many sequential tests over its lifetime.
pub type ServerReporter = Arc<dyn Fn(ServerEvent) + Send + Sync>;

/// Server certificate configuration for the native QUIC transport.
#[cfg(feature = "quic")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuicServerOptions {
    /// Generate an ephemeral self-signed benchmark certificate.
    pub self_signed: bool,
    /// PEM certificate chain for an explicitly configured server identity.
    pub cert_path: Option<std::path::PathBuf>,
    /// PEM private key matching `cert_path`.
    pub key_path: Option<std::path::PathBuf>,
}

/// Server-side configuration.
#[derive(Clone)]
pub struct ServerOptions {
    pub port: u16,
    pub transport: Transport,
    /// iroh only: bind a direct-only endpoint (no relay/discovery). Ignored by
    /// TCP/WS.
    pub direct_only: bool,
    /// Upper bound on a client-requested `time` (PARAM_EXCHANGE), in seconds.
    /// `protocol::params`'s own wire bound (86400s) is a JSON-payload sanity
    /// ceiling, not an operational one — the server waits `time + 10s` for
    /// TEST_END while holding the single-test lock, so an unauthenticated peer
    /// sending `{"time": 86400}` would otherwise deny service for a full day.
    /// Default 3600s (1h) is generous but bounded.
    pub max_test_seconds: u32,
    /// Optional live per-test reporter (interval throughput + completion). The
    /// library stays silent when `None`.
    pub on_event: Option<ServerReporter>,
    /// Native QUIC-only certificate configuration.
    #[cfg(feature = "quic")]
    pub quic: Option<QuicServerOptions>,
}

impl std::fmt::Debug for ServerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("ServerOptions");
        debug
            .field("port", &self.port)
            .field("transport", &self.transport)
            .field("direct_only", &self.direct_only)
            .field("max_test_seconds", &self.max_test_seconds)
            .field("on_event", &self.on_event.as_ref().map(|_| "<fn>"));
        #[cfg(feature = "quic")]
        debug.field("quic", &self.quic);
        debug.finish()
    }
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            port: 5201,
            transport: Transport::Tcp,
            direct_only: false,
            max_test_seconds: DEFAULT_MAX_TEST_SECONDS,
            on_event: None,
            #[cfg(feature = "quic")]
            quic: None,
        }
    }
}

impl ServerOptions {
    /// Rejects contradictory transport-specific options before binding.
    pub fn validate(&self) -> Result<()> {
        #[cfg(feature = "quic")]
        {
            if self.transport == Transport::Quic {
                let quic = self
                    .quic
                    .as_ref()
                    .ok_or_else(|| NetsuError::Protocol("missing QUIC server options".into()))?;
                let has_cert = quic.cert_path.is_some();
                let has_key = quic.key_path.is_some();
                if has_cert != has_key {
                    return Err(NetsuError::Protocol(
                        "QUIC server requires both certificate and key".into(),
                    ));
                }
                if quic.self_signed == (has_cert && has_key) {
                    return Err(NetsuError::Protocol(
                        "QUIC server requires exactly one of self-signed or certificate and key"
                            .into(),
                    ));
                }
            } else if self.quic.is_some() {
                return Err(NetsuError::Protocol(
                    "QUIC server options require Transport::Quic".into(),
                ));
            }
        }
        Ok(())
    }
}

/// A running server. Dropping it does not stop the server — call
/// [`NetsuServer::close`], which stops accepting, aborts any active test, and
/// releases the port.
pub struct NetsuServer {
    pub port: u16,
    /// `Some` for an iroh server: the `EndpointTicket` string a client dials
    /// with `--peer`. `None` for TCP/WS (addressed by `host:port`).
    pub endpoint_ticket: Option<String>,
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
    opts.validate()?;
    // iroh listens on a QUIC endpoint, not a TCP port — an entirely separate
    // accept path that demuxes control/data as bi-streams of one connection.
    #[cfg(feature = "iroh")]
    if matches!(opts.transport, Transport::Iroh) {
        return start_iroh_server(opts).await;
    }
    #[cfg(feature = "quic")]
    if matches!(opts.transport, Transport::Quic) {
        return start_quic_server(opts).await;
    }
    // Both remaining transports listen on TCP (WS is HTTP-over-TCP); only how an accepted
    // connection becomes a pipe differs. A ws-mode server never speaks plain
    // TCP and vice versa — official iperf3 simply can't connect to a ws port.
    // Bind all interfaces, not just loopback, so the server is reachable from
    // another host or container (the interop matrix connects across
    // containers). iperf3 and the TS server both bind the wildcard; tests
    // connect to 127.0.0.1, which the wildcard accepts.
    let listener = TcpListener::bind(("0.0.0.0", opts.port)).await?;
    let port = listener.local_addr()?.port();
    let transport = opts.transport;
    let core = Arc::new(ServerCore::new(
        port,
        opts.max_test_seconds,
        opts.on_event.clone(),
    ));
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
                    let core = core.clone();
                    // Detached per-connection task: `close()` does not await
                    // these, so a connection stuck in its 30s cookie read (or WS
                    // handshake) can never make `close()` hang. Dropping the
                    // listener above is what actually frees the port.
                    match transport {
                        Transport::Tcp => {
                            let pipe = TcpPipe::from_stream(stream);
                            tokio::spawn(async move {
                                core.handle_connection(pipe, |p: TcpPipe| {
                                    p.into_data_channel()
                                        .map(|c| Box::new(c) as Box<dyn DataChannel>)
                                }, || {})
                                .await;
                            });
                        }
                        #[cfg(feature = "ws")]
                        Transport::Ws => {
                            tokio::spawn(async move {
                                // The WS opening handshake is per-connection and
                                // async; run it inside the spawned task so a slow
                                // or non-upgrading peer can't stall the accept
                                // loop. Bound it with CONTROL_TIMEOUT so a peer
                                // that completes TCP but never sends the HTTP
                                // upgrade can't park this task (and its fd)
                                // forever — the TCP path's 30s cookie read
                                // bounds the same case, and the client side
                                // bounds it via WS_CONNECT_TIMEOUT.
                                match tokio::time::timeout(CONTROL_TIMEOUT, WsPipe::accept(stream))
                                    .await
                                {
                                    Ok(Ok(pipe)) => {
                                        core.handle_connection(pipe, |p: WsPipe<TcpStream>| {
                                            p.into_data_channel()
                                                .map(|c| Box::new(c) as Box<dyn DataChannel>)
                                        }, || {})
                                        .await;
                                    }
                                    Ok(Err(e)) => {
                                        eprintln!("netsu server: ws handshake failed: {e}");
                                    }
                                    Err(_) => {
                                        eprintln!("netsu server: ws handshake timed out");
                                    }
                                }
                            });
                        }
                        // iroh returns early from `start_server`, so this TCP
                        // accept loop never sees it.
                        #[cfg(feature = "iroh")]
                        Transport::Iroh => unreachable!("iroh uses start_iroh_server"),
                        #[cfg(feature = "quic")]
                        Transport::Quic => unreachable!("quic uses its own UDP endpoint"),
                    }
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
        endpoint_ticket: None,
        shutdown: shutdown_tx,
        accept_task,
    })
}

/// iroh server: bind a QUIC endpoint, then for each accepted connection accept
/// its bi-streams and feed each to [`ServerCore::handle_connection`] — the
/// first (cookie → no active test) becomes the control session, the rest
/// (cookie → active session's CREATE_STREAMS window) become data streams. This
/// reuses the exact TCP/WS classification, since every netsu stream carries a
/// cookie preamble.
#[cfg(feature = "iroh")]
async fn start_iroh_server(opts: ServerOptions) -> Result<NetsuServer> {
    use crate::p2p::{THROUGHPUT_ALPN, endpoint};
    use crate::transport::iroh::IrohPipe;

    let (endpoint, ticket) =
        endpoint::bind_listener_with_ticket(THROUGHPUT_ALPN, opts.direct_only, true)
            .await
            .map_err(|e| NetsuError::Protocol(format!("bind iroh server: {e:#}")))?;
    let core = Arc::new(ServerCore::new(
        0,
        opts.max_test_seconds,
        opts.on_event.clone(),
    ));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                incoming = endpoint.accept() => {
                    // `None` means the endpoint was closed.
                    let Some(incoming) = incoming else { break };
                    let core = core.clone();
                    tokio::spawn(async move {
                        let connection = match incoming.await {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        // One task per bi-stream so the control session (long-lived)
                        // does not block accepting this connection's data streams.
                        // The loop ends when `accept_bi` errors (connection closed).
                        while let Ok((send, recv)) = connection.accept_bi().await {
                            let core = core.clone();
                            tokio::spawn(async move {
                                let pipe = IrohPipe::new(send, recv);
                                core.handle_connection(pipe, |p: IrohPipe| {
                                    p.into_data_channel()
                                        .map(|c| Box::new(c) as Box<dyn DataChannel>)
                                }, || {})
                                .await;
                            });
                        }
                    });
                }
            }
        }
        core.abort();
        endpoint.close().await;
    });

    Ok(NetsuServer {
        port: 0,
        endpoint_ticket: Some(ticket),
        shutdown: shutdown_tx,
        accept_task,
    })
}

/// Native QUIC server: one Quinn connection per test, with every client-opened
/// bidirectional stream classified through the existing cookie/session core.
#[cfg(feature = "quic")]
async fn start_quic_server(opts: ServerOptions) -> Result<NetsuServer> {
    use crate::transport::quic::STREAM_POLICY_ERROR;
    use crate::transport::quic::channel::QuicPipe;
    use crate::transport::quic::endpoint::QuicEndpoint;

    let quic_options = opts
        .quic
        .as_ref()
        .ok_or_else(|| NetsuError::Protocol("missing QUIC server options".into()))?;
    let (config, _certificate) = crate::transport::quic::tls::server_config(quic_options)?;
    let endpoint = QuicEndpoint::bind_server(
        std::net::SocketAddr::from(([0, 0, 0, 0], opts.port)),
        config,
    )?;
    let port = endpoint.local_addr()?.port();
    let core = Arc::new(ServerCore::new(
        port,
        opts.max_test_seconds,
        opts.on_event.clone(),
    ));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                accepted = endpoint.accept() => {
                    let (connection, _) = match accepted {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    let core = core.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                accepted = connection.accept_bi() => {
                                    let (send, receive) = match accepted {
                                        Ok(stream) => stream,
                                        Err(_) => break,
                                    };
                                    let core = core.clone();
                                    let busy_connection = connection.clone();
                                    tokio::spawn(async move {
                                        let pipe = QuicPipe::new(send, receive);
                                        core.handle_connection(
                                            pipe,
                                            |pipe: QuicPipe| {
                                                Ok(Box::new(pipe.into_data_channel())
                                                    as Box<dyn DataChannel>)
                                            },
                                            move || {
                                                busy_connection.close(
                                                    quinn::VarInt::from_u32(STREAM_POLICY_ERROR),
                                                    b"netsu: unexpected or excess stream",
                                                );
                                            },
                                        )
                                        .await;
                                    });
                                }
                                accepted = connection.accept_uni() => {
                                    if accepted.is_ok() {
                                        connection.close(
                                            quinn::VarInt::from_u32(STREAM_POLICY_ERROR),
                                            b"netsu: unidirectional streams are forbidden",
                                        );
                                    }
                                    break;
                                }
                            }
                        }
                    });
                }
            }
        }
        core.abort();
        endpoint.close().await;
    });

    Ok(NetsuServer {
        port,
        endpoint_ticket: None,
        shutdown: shutdown_tx,
        accept_task,
    })
}

/// The single-test lock plus the accept rule. One [`ServerCore`] per listening
/// port; at most one [`ServerSession`] is active at a time.
struct ServerCore {
    /// The listening port. UDP data sockets bind this same port (UDP is
    /// data-plane only; the control channel is the TCP connection on it).
    port: u16,
    max_test_seconds: u32,
    on_event: Option<ServerReporter>,
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
    /// Number of data streams still legal in the current CREATE_STREAMS
    /// window. Claiming is atomic so concurrent QUIC streams cannot both pass
    /// the final slot.
    remaining_streams: AtomicU32,
    /// Delivers a newly-accepted data channel to the running session.
    stream_tx: mpsc::UnboundedSender<Box<dyn DataChannel>>,
    /// Fired by `close()`/abort to make the session's control loop return.
    abort: watch::Sender<bool>,
}

impl SessionShared {
    fn try_claim_stream(&self) -> bool {
        self.awaiting.load(Ordering::Acquire)
            && self
                .remaining_streams
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
    }
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
    fn new(port: u16, max_test_seconds: u32, on_event: Option<ServerReporter>) -> Self {
        ServerCore {
            port,
            max_test_seconds,
            on_event,
            active: StdMutex::new(None),
        }
    }

    /// The accept rule (`PROTOCOL.md`): read the 37-byte cookie; if no test is
    /// active this is a new control connection; if a test is active and the
    /// cookie matches during its CREATE_STREAMS window it is a data stream;
    /// otherwise reply ACCESS_DENIED and close.
    async fn handle_connection<P, F, B>(&self, mut pipe: P, to_channel: F, on_busy: B)
    where
        P: BytePipe,
        F: FnOnce(P) -> Result<Box<dyn DataChannel>>,
        B: FnOnce(),
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
                Some(h) if h.cookie == cookie && h.shared.try_claim_stream() => {
                    Decision::Stream(h.shared.clone())
                }
                Some(_) => Decision::Busy,
                None => {
                    let (stream_tx, stream_rx) = mpsc::unbounded_channel();
                    let (abort, abort_rx) = watch::channel(false);
                    let shared = Arc::new(SessionShared {
                        awaiting: AtomicBool::new(false),
                        remaining_streams: AtomicU32::new(0),
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
                on_busy();
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
                    self.port,
                    self.on_event.clone(),
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
/// The transport half of a server-side data stream — the mirror of the
/// client's `StreamIo`. TCP and WS streams both use the `DataChannel`
/// byte-stream trait (`Channel`). A reverse-mode (server-sends) UDP stream
/// holds its socket until [`ServerSession::start_running`] moves it into the
/// sender task; a forward-mode (server-receives) UDP stream's receiver takes
/// the socket at accept time, so its variant is already `None`.
enum ServerStreamIo {
    Channel(SharedChannel),
    Udp(Option<UdpSocket>),
}

struct ServerStream {
    counters: SharedCounters,
    io: ServerStreamIo,
    task: Option<JoinHandle<()>>,
    closed: bool,
    /// Snapshot of `channel.error()` taken at close time, before we tear the
    /// channel down ourselves — see `client.rs`'s `StreamState::latched_error`
    /// for the full rationale (teardown noise must not be reported as a
    /// genuine transfer failure). Always `None` for UDP: a UDP send error or
    /// lost packet is counted, never a fatal transfer failure.
    latched_error: Option<String>,
}

impl ServerStream {
    async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let ServerStreamIo::Channel(channel) = &self.io {
            let mut ch = channel.lock().await;
            self.latched_error = ch.error().map(|e| e.to_string());
            ch.close().await;
        }
    }
}

/// All mutable state for one server-side test session.
struct ServerSession {
    shared: Arc<SessionShared>,
    stream_rx: mpsc::UnboundedReceiver<Box<dyn DataChannel>>,
    abort_rx: watch::Receiver<bool>,
    max_test_seconds: u32,
    on_event: Option<ServerReporter>,
    /// The listening port, so UDP data sockets bind the same one.
    port: u16,

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
        port: u16,
        on_event: Option<ServerReporter>,
    ) -> Self {
        let (stop_senders, stop_senders_rx) = watch::channel(false);
        let (stop_receivers, stop_receivers_rx) = watch::channel(false);
        ServerSession {
            shared,
            stream_rx,
            abort_rx,
            max_test_seconds,
            on_event,
            port,
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
            // Reverse UDP = the server sends. Refuse up front (SERVER_ERROR +
            // logged reason) if this host cannot emit a usable datagram at the
            // negotiated size — either because nothing is sendable, or because
            // `len` is below the 12-byte header (params allows `len >= 4`, but a
            // peer sending `{reverse, len: 8}` would otherwise reach the sender).
            // Better a clean SERVER_ERROR than streams that transfer zero bytes.
            // The client chooses `len`, so this is remotely reachable.
            if params.reverse && probe_max_udp_send_len(params.len).await < UDP_HEADER_SIZE {
                return Err(NetsuError::Protocol(
                    "cannot send a usable UDP datagram at the requested len — refusing reverse UDP test".into(),
                ));
            }
            // The first UDP bind MUST complete before CREATE_STREAMS is
            // announced: iperf3 clients send their hello exactly once, with no
            // retry, immediately on seeing CREATE_STREAMS. `awaiting` stays
            // false — UDP data streams never arrive via the TCP accept loop, so
            // a stray TCP connection during a UDP test is correctly ACCESS_DENIED'd.
            let first = udp_server_bind(self.port).await?;
            write_state(pipe, CREATE_STREAMS).await?;
            self.collect_udp_streams(&params, first).await?;
        } else {
            // Open the CREATE_STREAMS window before announcing it, so a
            // data-stream connection that races in the instant after the client
            // sees CREATE_STREAMS is recognized rather than rejected as a stray.
            self.shared
                .remaining_streams
                .store(params.parallel, Ordering::Release);
            self.shared.awaiting.store(true, Ordering::Release);
            write_state(pipe, CREATE_STREAMS).await?;
            self.collect_streams(params.parallel, &params).await?;
            self.shared.awaiting.store(false, Ordering::Release);
        }

        write_state(pipe, TEST_START).await?;
        self.running = true;
        self.start_instant = Some(Instant::now());
        write_state(pipe, TEST_RUNNING).await?;
        self.start_running(&params);

        // Safety cap: the client owns the real duration timer; the server just
        // waits for TEST_END with a +10s grace, aborting if `close()` fires.
        let wait = Duration::from_secs(params.time as u64) + TEST_RUNNING_GRACE;
        // While waiting for TEST_END, tick once a second to emit the server's
        // view of throughput (iperf3 shows this on both sides). The meter is fed
        // by the receiver/sender tasks; snapshotting it gives per-interval bytes.
        let reporter = self.on_event.clone();
        let meter = self.meter.clone();
        let deadline = Instant::now() + wait;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.tick().await; // consume the immediate first tick
        // Recreate the 1-byte state read each iteration (cancel-safe: a tick that
        // interrupts it drops it before any byte is consumed), so `pipe`'s borrow
        // is released each loop and stays available for the results handshake.
        let state = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::select! {
                biased;
                _ = self.abort_rx.changed() => return Err(NetsuError::Protocol("aborted".into())),
                st = read_state(pipe, Some(remaining)) => break st?,
                _ = ticker.tick() => {
                    if let Some(reporter) = &reporter {
                        let report = meter.lock().await.snap(Instant::now());
                        reporter(ServerEvent::Interval(report));
                    }
                }
            }
        };
        self.running = false;
        self.end_instant = Some(Instant::now());
        if state != TEST_END {
            return Err(NetsuError::Protocol(format!(
                "expected TEST_END, got {state}"
            )));
        }

        // Emit the final server-side summary.
        if let Some(reporter) = &self.on_event {
            let dur = match (self.start_instant, self.end_instant) {
                (Some(s), Some(e)) => e.duration_since(s).as_secs_f64(),
                _ => 0.0,
            };
            let bytes = self.meter.lock().await.total_bytes();
            reporter(ServerEvent::Complete {
                duration_seconds: dur,
                bytes,
                bits_per_second: crate::stats::bits_per_second(bytes, dur),
            });
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
            io: ServerStreamIo::Channel(channel),
            task,
            closed: false,
            latched_error: None,
        });
    }

    /// Accepts `parallel` UDP streams via iperf3's rebind trick (`PROTOCOL.md`
    /// "UDP specifics"): `first` is already bound (before CREATE_STREAMS).
    /// Accept a hello on it (which `connect()`s it to that stream's peer);
    /// then, if more streams remain, bind a fresh SO_REUSEADDR socket on the
    /// same port for the next one — *before* replying to this stream's hello,
    /// closing the window where a fast client's next hello finds nothing bound.
    async fn collect_udp_streams(&mut self, params: &TestParams, first: UdpSocket) -> Result<()> {
        // `Option` + per-iteration `take()`: the accept future consumes the
        // listen socket, and we rebind for the next stream, so a plain moved
        // loop variable won't satisfy the borrow checker across iterations.
        let mut pending: Option<UdpSocket> = Some(first);
        for i in 0..params.parallel {
            let listener = match pending.take() {
                Some(s) => s,
                None => {
                    return Err(NetsuError::Protocol(
                        "internal: missing udp accept socket".into(),
                    ));
                }
            };
            let stream_sock = tokio::select! {
                biased;
                _ = self.abort_rx.changed() => return Err(NetsuError::Protocol("aborted".into())),
                r = udp_server_accept(listener, CONTROL_TIMEOUT) => r?,
            };
            // Bind the next listener BEFORE replying, per PROTOCOL.md ordering.
            if i + 1 < params.parallel {
                pending = Some(udp_server_bind(self.port).await?);
            }
            udp_server_send_reply(&stream_sock).await?;
            self.add_udp_stream(stream_sock, params);
        }
        Ok(())
    }

    /// Adds one UDP stream. In forward mode (server receives) the receiver task
    /// is attached immediately; in reverse mode (server sends) the socket is
    /// held until [`ServerSession::start_running`].
    fn add_udp_stream(&mut self, socket: UdpSocket, params: &TestParams) {
        let id = next_stream_id(self.streams.len());
        let counters: SharedCounters = Arc::new(Mutex::new(StreamCounters::new(id)));
        let (io, task) = if params.reverse {
            (ServerStreamIo::Udp(Some(socket)), None)
        } else {
            let task = tokio::spawn(run_udp_receiver(
                socket,
                counters.clone(),
                self.meter.clone(),
                self.stop_receivers_rx.clone(),
            ));
            (ServerStreamIo::Udp(None), Some(task))
        };
        self.streams.push(ServerStream {
            counters,
            io,
            task,
            closed: false,
            latched_error: None,
        });
    }

    /// Reverse mode only: start a sender task per stream. Forward-mode
    /// receivers were already attached at stream-add time.
    fn start_running(&mut self, params: &TestParams) {
        if !params.reverse {
            return;
        }
        let len = params.len;
        let bandwidth = params.bandwidth;
        let meter = self.meter.clone();
        let stop_rx = self.stop_senders_rx.clone();
        for s in &mut self.streams {
            s.task = Some(match &mut s.io {
                ServerStreamIo::Channel(channel) => tokio::spawn(run_sender(
                    channel.clone(),
                    s.counters.clone(),
                    meter.clone(),
                    len,
                    stop_rx.clone(),
                )),
                ServerStreamIo::Udp(socket) => match socket.take() {
                    Some(sock) => tokio::spawn(run_udp_sender(
                        sock,
                        s.counters.clone(),
                        meter.clone(),
                        len,
                        bandwidth,
                        stop_rx.clone(),
                    )),
                    None => continue,
                },
            });
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
