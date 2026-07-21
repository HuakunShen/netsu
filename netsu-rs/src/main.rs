//! netsu CLI — iperf3-style flags over the library. Kept thin: argument
//! parsing, validation, output formatting, and process lifecycle only; all
//! measurement logic lives in the `netsu` library.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, Subcommand};

#[cfg(feature = "iroh")]
mod mux_cli;
#[cfg(feature = "tui")]
mod tui;

use netsu::client::{
    ClientOptions, ConnectionInfo, TestResult, Transport, connection_json, run_client,
};
use netsu::error::NetsuError;
use netsu::format::{format_bits, format_bytes, interval_line, parse_bandwidth, parse_len};
use netsu::server::{ServerOptions, start_server};
use netsu::stats::IntervalReport;

#[derive(Parser)]
#[command(
    name = "netsu",
    version,
    about = "iperf3-compatible network speed test"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a netsu speed test server.
    Server(ServerArgs),
    /// Run a speed test against a netsu/iperf3 server.
    Client(ClientArgs),
    /// Multiplexing / priority latency lab (iroh).
    #[cfg(feature = "iroh")]
    Mux(mux_cli::MuxArgs),
    /// Interactive terminal UI: host/join a cross-device test by sharing a
    /// short code (tcp/udp/ws/iroh), plus the kbm sharing demo — no flags.
    #[cfg(feature = "tui")]
    Tui,
}

#[derive(Parser)]
struct ServerArgs {
    /// Listening port.
    #[arg(short = 'p', long, default_value_t = 5201)]
    port: u16,
    /// Use the WebSocket transport (netsu-only).
    #[arg(long)]
    ws: bool,
    /// Use the iroh/QUIC transport (netsu-only). Prints a ticket to dial.
    #[arg(long)]
    iroh: bool,
    /// Use fixed-address native QUIC (netsu-only).
    #[arg(long)]
    quic: bool,
    /// Use direct-only WebRTC DataChannels (netsu-only).
    #[arg(long)]
    webrtc: bool,
    /// WebRTC only: HTTP(S) signaling service base URL.
    #[arg(long)]
    signal_url: Option<String>,
    /// WebRTC only: STUN discovery URL; may be repeated up to four times.
    #[arg(long)]
    stun: Vec<String>,
    /// WebRTC only: include selected candidate addresses in diagnostics.
    #[arg(long)]
    include_addresses: bool,
    /// QUIC only: generate an ephemeral benchmark certificate.
    #[arg(long)]
    quic_self_signed: bool,
    /// QUIC only: PEM server certificate chain.
    #[arg(long)]
    quic_cert: Option<std::path::PathBuf>,
    /// QUIC only: PEM private key matching --quic-cert.
    #[arg(long)]
    quic_key: Option<std::path::PathBuf>,
    /// iroh only: bind a direct-only endpoint (no relay/discovery). Skips
    /// hole-punching, so the peer must reach this endpoint directly — a server
    /// behind a strict inbound firewall (e.g. Windows) is unreachable this way;
    /// use the default mode (omit this) so hole-punching/relay can traverse it.
    #[arg(long)]
    direct_only: bool,
    /// iroh only: don't publish a rendez-key short code (print only the ticket).
    #[arg(long)]
    no_rendezkey: bool,
    /// iroh only: rendez-key base URL.
    #[arg(long)]
    rendezkey_url: Option<String>,
    /// iroh only: rendez-key code time-to-live, in seconds.
    #[arg(long, default_value_t = 3600)]
    rendezkey_ttl: u64,
    /// iroh only: how many times the rendez-key code may be claimed (so one
    /// code serves several reconnects). Open mode caps this at 5.
    #[arg(long, default_value_t = 5)]
    rendezkey_reads: u32,
}

#[derive(Parser)]
struct ClientArgs {
    /// Server host.
    host: String,
    /// Server port.
    #[arg(short = 'p', long, default_value_t = 5201)]
    port: u16,
    /// Duration in seconds.
    #[arg(short = 't', long = "time", default_value_t = 10)]
    time: u32,
    /// Use UDP.
    #[arg(short = 'u', long)]
    udp: bool,
    /// Use the WebSocket transport (netsu-only).
    #[arg(long)]
    ws: bool,
    /// Use the iroh/QUIC transport (netsu-only). HOST is then a ticket/code.
    #[arg(long)]
    iroh: bool,
    /// Use fixed-address native QUIC (netsu-only).
    #[arg(long)]
    quic: bool,
    /// Use direct-only WebRTC DataChannels; HOST is the room code.
    #[arg(long)]
    webrtc: bool,
    /// WebRTC only: HTTP(S) signaling service base URL.
    #[arg(long)]
    signal_url: Option<String>,
    /// WebRTC only: STUN discovery URL; may be repeated up to four times.
    #[arg(long)]
    stun: Vec<String>,
    /// WebRTC only: include selected candidate addresses in diagnostics.
    #[arg(long)]
    include_addresses: bool,
    /// QUIC only: explicitly disable certificate authentication for benchmarks.
    #[arg(long)]
    quic_insecure: bool,
    /// QUIC only: PEM CA used to authenticate the server.
    #[arg(long)]
    quic_ca: Option<std::path::PathBuf>,
    /// iroh only: require a direct path (fail if the connection uses a relay).
    #[arg(long)]
    direct_only: bool,
    /// iroh only: treat HOST as a literal ticket (don't claim a rendez-key code).
    #[arg(long)]
    no_rendezkey: bool,
    /// iroh only: rendez-key base URL used to claim a short code.
    #[arg(long)]
    rendezkey_url: Option<String>,
    /// Number of parallel streams.
    #[arg(short = 'P', long, default_value_t = 1)]
    parallel: u32,
    /// Server sends, client receives.
    #[arg(short = 'R', long)]
    reverse: bool,
    /// Target bandwidth, e.g. 5M (UDP pacing, bits/s; K/M/G decimal).
    #[arg(short = 'b', long)]
    bandwidth: Option<String>,
    /// Read/write block size, e.g. 128K (bytes; K/M/G are 1024-based).
    #[arg(short = 'l', long)]
    len: Option<String>,
    /// Seconds between periodic reports (0 disables).
    #[arg(short = 'i', long, default_value_t = 1)]
    interval: u32,
    /// Output results as JSON (stdout carries nothing else).
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() {
    // clap prints its own parse errors to stderr and exits non-zero, so
    // `--json`'s stdout-purity contract holds for usage errors too.
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Server(a) => run_server(a).await,
        Cmd::Client(a) => run_client_cmd(a).await,
        #[cfg(feature = "iroh")]
        Cmd::Mux(a) => mux_cli::run(a).await,
        #[cfg(feature = "tui")]
        Cmd::Tui => match tui::run().await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("netsu tui: {e:#}");
                1
            }
        },
    };
    std::process::exit(code);
}

/// Surfaces the library's phase-tagged message (e.g. "server busy") rather than
/// a debug-formatted enum.
fn describe(err: &NetsuError) -> String {
    match err {
        NetsuError::ServerBusy => "server busy (ACCESS_DENIED)".to_string(),
        NetsuError::ServerError => "server reported an error (SERVER_ERROR)".to_string(),
        other => other.to_string(),
    }
}

const EXIT_RUNTIME: i32 = 1;
const EXIT_CONFIG: i32 = 2;
const EXIT_SETUP_TIMEOUT: i32 = 3;
const EXIT_DIRECT_UNAVAILABLE: i32 = 4;
const WEBRTC_DIRECT_WARNING: &str = "warning: WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run";

fn is_direct_path_unavailable(error: &NetsuError) -> bool {
    matches!(
        error,
        NetsuError::Setup {
            transport: "webrtc",
            detail,
            ..
        } if detail == "direct path is unavailable"
    )
}

fn is_setup_timeout(error: &NetsuError) -> bool {
    matches!(
        error,
        NetsuError::Setup {
            transport: "webrtc",
            detail,
            ..
        } if detail.contains("timed out")
    )
}

fn runtime_exit_code(error: &NetsuError) -> i32 {
    if is_direct_path_unavailable(error) {
        EXIT_DIRECT_UNAVAILABLE
    } else if is_setup_timeout(error) {
        EXIT_SETUP_TIMEOUT
    } else {
        EXIT_RUNTIME
    }
}

fn setup_error_json(error: &NetsuError) -> Option<String> {
    let NetsuError::Setup {
        transport,
        phase,
        detail,
    } = error
    else {
        return None;
    };
    if *transport != "webrtc" {
        return None;
    }
    let kind = if is_direct_path_unavailable(error) {
        "direct_path_unavailable"
    } else if is_setup_timeout(error) {
        "setup_timeout"
    } else {
        "setup_failed"
    };
    serde_json::to_string(&serde_json::json!({
        "error": {
            "transport": transport,
            "phase": phase.to_string(),
            "kind": kind,
            "message": format!("{transport} setup failed during {phase}: {detail}"),
        }
    }))
    .ok()
}

enum CliFailure {
    Config(String),
    Runtime(NetsuError),
    RuntimeMessage(String),
}

impl CliFailure {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) => EXIT_CONFIG,
            Self::Runtime(error) => runtime_exit_code(error),
            Self::RuntimeMessage(_) => EXIT_RUNTIME,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Config(message) | Self::RuntimeMessage(message) => message.clone(),
            Self::Runtime(error) => describe(error),
        }
    }
}

/// Resolve optional transports against compiled-in features. Their flags stay valid
/// flags even without their feature so the error is actionable rather than a
/// clap "unknown argument".
fn select_transport(ws: bool, iroh: bool, quic: bool, webrtc: bool) -> Result<Transport, String> {
    if [ws, iroh, quic, webrtc]
        .into_iter()
        .filter(|selected| *selected)
        .count()
        > 1
    {
        return Err("--ws, --iroh, --quic, and --webrtc are mutually exclusive".to_string());
    }
    if ws {
        #[cfg(feature = "ws")]
        {
            return Ok(Transport::Ws);
        }
        #[cfg(not(feature = "ws"))]
        {
            return Err("ws support not compiled in; rebuild with --features ws".to_string());
        }
    }
    if iroh {
        #[cfg(feature = "iroh")]
        {
            return Ok(Transport::Iroh);
        }
        #[cfg(not(feature = "iroh"))]
        {
            return Err("iroh support not compiled in; rebuild with --features iroh".to_string());
        }
    }
    if quic {
        #[cfg(feature = "quic")]
        {
            return Ok(Transport::Quic);
        }
        #[cfg(not(feature = "quic"))]
        {
            return Err("quic support not compiled in; rebuild with --features quic".to_string());
        }
    }
    if webrtc {
        #[cfg(feature = "webrtc")]
        {
            return Ok(Transport::WebRtc);
        }
        #[cfg(not(feature = "webrtc"))]
        {
            return Err(
                "webrtc support not compiled in; rebuild with --features webrtc".to_string(),
            );
        }
    }
    Ok(Transport::Tcp)
}

fn validate_webrtc_args(
    enabled: bool,
    signal_url: Option<&str>,
    stun: &[String],
    include_addresses: bool,
) -> Result<(), String> {
    if !enabled {
        if signal_url.is_some() || !stun.is_empty() || include_addresses {
            return Err("--signal-url, --stun, and --include-addresses require --webrtc".into());
        }
        return Ok(());
    }
    if signal_url.is_none() {
        return Err("--webrtc requires --signal-url <HTTP(S)_URL>".into());
    }
    Ok(())
}

#[cfg(feature = "webrtc")]
fn build_webrtc_options(
    signal_url: Option<&str>,
    stun: &[String],
    include_addresses: bool,
) -> Result<netsu::transport::webrtc::WebRtcOptions, String> {
    let signal_url = signal_url.ok_or_else(|| "--webrtc requires --signal-url".to_string())?;
    netsu::transport::webrtc::WebRtcOptions::new(signal_url, stun, include_addresses)
        .map_err(|error| error.to_string())
}

fn validate_quic_server_args(args: &ServerArgs) -> Result<(), String> {
    let has_cert = args.quic_cert.is_some();
    let has_key = args.quic_key.is_some();
    if !args.quic {
        if args.quic_self_signed || has_cert || has_key {
            return Err("QUIC certificate flags require --quic".to_string());
        }
        return Ok(());
    }
    if has_cert != has_key {
        return Err("QUIC server requires both --quic-cert and --quic-key".to_string());
    }
    if args.quic_self_signed == (has_cert && has_key) {
        return Err(
            "QUIC server certificate mode requires exactly one of --quic-self-signed or --quic-cert/--quic-key"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_quic_client_args(args: &ClientArgs) -> Result<(), String> {
    if !args.quic {
        if args.quic_insecure || args.quic_ca.is_some() {
            return Err("QUIC trust flags require --quic".to_string());
        }
        return Ok(());
    }
    if args.quic_insecure == args.quic_ca.is_some() {
        return Err(
            "QUIC client trust mode requires exactly one of --quic-insecure or --quic-ca"
                .to_string(),
        );
    }
    Ok(())
}

/// Publish the iroh ticket as a short rendez-key code (best-effort — a failure
/// just falls back to the printed ticket). Works anonymously in open mode; a
/// token (if set) unlocks the privileged tier.
#[cfg(feature = "iroh")]
async fn publish_rendezkey_code(ticket: &str, a: &ServerArgs) {
    use netsu::p2p::rendezkey;
    let url = a
        .rendezkey_url
        .as_deref()
        .unwrap_or(rendezkey::DEFAULT_BASE_URL);
    let token = rendezkey::token_from_env();
    match rendezkey::store(
        url,
        token.as_deref(),
        ticket,
        a.rendezkey_ttl,
        a.rendezkey_reads,
    )
    .await
    {
        Ok(code) => println!(
            "code:   {code}   (share this — expires in ~{}m)",
            a.rendezkey_ttl / 60
        ),
        Err(e) => {
            eprintln!("netsu server: rendez-key unavailable ({e:#}); share the ticket instead")
        }
    }
}

/// Resolve the client's positional peer into a ticket string: for iroh, a short
/// rendez-key code is claimed into a ticket; a full ticket passes through.
#[allow(unused_variables)]
async fn resolve_peer_host(a: &ClientArgs) -> Result<String, String> {
    #[cfg(feature = "iroh")]
    if a.iroh && !a.no_rendezkey {
        use netsu::p2p::{addr, rendezkey};
        let url = a
            .rendezkey_url
            .as_deref()
            .unwrap_or(rendezkey::DEFAULT_BASE_URL);
        return addr::resolve_ticket(&a.host, url)
            .await
            .map_err(|e| format!("{e:#}"));
    }
    Ok(a.host.clone())
}

async fn run_server(a: ServerArgs) -> i32 {
    let transport = match select_transport(a.ws, a.iroh, a.quic, a.webrtc) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("netsu server: {e}");
            return EXIT_CONFIG;
        }
    };
    if let Err(error) = validate_quic_server_args(&a) {
        eprintln!("netsu server: {error}");
        return EXIT_CONFIG;
    }
    if let Err(error) = validate_webrtc_args(
        a.webrtc,
        a.signal_url.as_deref(),
        &a.stun,
        a.include_addresses,
    ) {
        eprintln!("netsu server: {error}");
        return EXIT_CONFIG;
    }
    #[cfg(feature = "webrtc")]
    let webrtc = if a.webrtc {
        match build_webrtc_options(a.signal_url.as_deref(), &a.stun, a.include_addresses) {
            Ok(options) => Some(options),
            Err(error) => {
                eprintln!("netsu server: {error}");
                return EXIT_CONFIG;
            }
        }
    } else {
        None
    };
    // Print the server's view of throughput per interval + a final summary, so
    // both ends show a speed log (as iperf3 does).
    let on_event: netsu::server::ServerReporter = std::sync::Arc::new(|ev| {
        use netsu::server::ServerEvent;
        match ev {
            ServerEvent::Interval(r) => println!("{}", interval_line(&r)),
            ServerEvent::Complete {
                duration_seconds,
                bytes,
                bits_per_second,
            } => {
                println!("- - - - - - - - - - - - - - - - - - - - - - - - -");
                println!(
                    "[SUM]   0.00-{duration_seconds:.2} sec  {:>12}  {:>14}  receiver",
                    format_bytes(bytes),
                    format_bits(bits_per_second)
                );
            }
        }
    });
    let server = match start_server(ServerOptions {
        port: a.port,
        transport,
        direct_only: a.direct_only,
        on_event: Some(on_event),
        #[cfg(feature = "quic")]
        quic: a.quic.then(|| netsu::server::QuicServerOptions {
            self_signed: a.quic_self_signed,
            cert_path: a.quic_cert.clone(),
            key_path: a.quic_key.clone(),
        }),
        #[cfg(feature = "webrtc")]
        webrtc,
        ..Default::default()
    })
    .await
    {
        Ok(s) => s,
        Err(e) => {
            if is_direct_path_unavailable(&e) {
                eprintln!("{WEBRTC_DIRECT_WARNING}");
            } else {
                eprintln!("netsu server: {}", describe(&e));
            }
            return runtime_exit_code(&e);
        }
    };
    match &server.endpoint_ticket {
        Some(code) if a.webrtc => {
            println!("netsu server listening (webrtc)");
            println!("code: {code}");
        }
        // iroh: the client dials this via `--peer`/positional HOST — a short
        // rendez-key code (hand-typable) or the full ticket.
        Some(ticket) => {
            println!("netsu server listening (iroh)");
            #[cfg(feature = "iroh")]
            if !a.no_rendezkey {
                publish_rendezkey_code(ticket, &a).await;
            }
            println!("ticket: {ticket}");
        }
        None => println!(
            "netsu server listening on {} ({})",
            server.port,
            if a.ws {
                "ws"
            } else if a.quic {
                "quic"
            } else if a.webrtc {
                "webrtc"
            } else {
                "tcp"
            }
        ),
    }
    // The listening server holds the runtime open; wait for Ctrl-C/SIGTERM,
    // then release the port cleanly instead of being killed out from under it.
    wait_for_shutdown().await;
    server.close().await;
    0
}

async fn run_client_cmd(a: ClientArgs) -> i32 {
    let json = a.json;
    match run_client_inner(a).await {
        Ok(()) => 0,
        Err(failure) => {
            let exit_code = failure.exit_code();
            let direct_unavailable = matches!(
                &failure,
                CliFailure::Runtime(error) if is_direct_path_unavailable(error)
            );
            let json_error = match &failure {
                CliFailure::Runtime(error) if json => setup_error_json(error),
                _ => None,
            };
            if direct_unavailable {
                eprintln!("{WEBRTC_DIRECT_WARNING}");
            }
            if let Some(value) = json_error {
                println!("{value}");
            } else if !direct_unavailable {
                eprintln!("netsu client: {}", failure.message());
            }
            exit_code
        }
    }
}

async fn run_client_inner(a: ClientArgs) -> Result<(), CliFailure> {
    let transport = select_transport(a.ws, a.iroh, a.quic, a.webrtc).map_err(CliFailure::Config)?;
    validate_quic_client_args(&a).map_err(CliFailure::Config)?;
    validate_webrtc_args(
        a.webrtc,
        a.signal_url.as_deref(),
        &a.stun,
        a.include_addresses,
    )
    .map_err(CliFailure::Config)?;
    if a.udp && a.ws {
        return Err(CliFailure::Config(
            "--udp and --ws are mutually exclusive".to_string(),
        ));
    }
    if a.udp && a.iroh {
        return Err(CliFailure::Config(
            "--udp and --iroh are mutually exclusive (iroh is reliable)".to_string(),
        ));
    }
    if a.udp && a.quic {
        return Err(CliFailure::Config(
            "--udp and --quic are mutually exclusive (QUIC is reliable)".to_string(),
        ));
    }
    if a.udp && a.webrtc {
        return Err(CliFailure::Config(
            "--udp and --webrtc are mutually exclusive (WebRTC is reliable)".to_string(),
        ));
    }
    if a.time < 1 {
        return Err(CliFailure::Config(format!(
            "invalid time: {} (must be >= 1)",
            a.time
        )));
    }
    if a.parallel < 1 {
        return Err(CliFailure::Config(format!(
            "invalid parallel: {} (must be >= 1)",
            a.parallel
        )));
    }
    let bandwidth = match a.bandwidth.as_deref() {
        Some(s) => Some(parse_bandwidth(s).map_err(|error| CliFailure::Config(error.to_string()))?),
        None => None,
    };
    let len = match a.len.as_deref() {
        Some(s) => Some(parse_len(s).map_err(|error| CliFailure::Config(error.to_string()))?),
        None => None,
    };
    #[cfg(feature = "webrtc")]
    let webrtc = if a.webrtc {
        Some(
            build_webrtc_options(a.signal_url.as_deref(), &a.stun, a.include_addresses)
                .map_err(CliFailure::Config)?,
        )
    } else {
        None
    };

    let json = a.json;
    let intervals: Arc<Mutex<Vec<IntervalReport>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = intervals.clone();
    let on_interval: Box<dyn FnMut(IntervalReport) + Send> = Box::new(move |r| {
        // --json must emit nothing but the final JSON blob on stdout.
        if !json {
            println!("{}", interval_line(&r));
        }
        if let Ok(mut v) = sink.lock() {
            v.push(r);
        }
    });

    let opts = ClientOptions {
        port: a.port,
        transport,
        udp: a.udp,
        reverse: a.reverse,
        duration: a.time,
        parallel: a.parallel,
        len,
        bandwidth,
        interval: (a.interval > 0).then(|| Duration::from_secs(a.interval as u64)),
        direct_only: a.direct_only,
        #[cfg(feature = "quic")]
        quic: a.quic.then(|| netsu::client::QuicClientOptions {
            insecure: a.quic_insecure,
            ca_path: a.quic_ca.clone(),
        }),
        #[cfg(feature = "webrtc")]
        webrtc,
    };

    let peer = resolve_peer_host(&a)
        .await
        .map_err(CliFailure::RuntimeMessage)?;
    if a.quic_insecure {
        eprintln!("warning: QUIC certificate verification disabled by explicit --quic-insecure");
    }
    let result = run_client(&peer, opts, Some(on_interval))
        .await
        .map_err(CliFailure::Runtime)?;

    if a.json {
        let intervals = intervals
            .lock()
            .map_err(|_| CliFailure::RuntimeMessage("interval lock poisoned".into()))?;
        println!("{}", to_json(&result, &intervals));
    } else {
        print_summary(&result);
    }
    Ok(())
}

fn print_summary(r: &TestResult) {
    let dur = format!("{:.2}", r.duration_seconds);
    println!("- - - - - - - - - - - - - - - - - - - - - - - - -");
    println!(
        "[SUM]   0.00-{dur} sec  {:>12}  {:>14}  sender",
        format_bytes(r.sent_bytes),
        format_bits(r.send_bits_per_second)
    );
    println!(
        "[SUM]   0.00-{dur} sec  {:>12}  {:>14}  receiver",
        format_bytes(r.received_bytes),
        format_bits(r.receive_bits_per_second)
    );
    if let Some(u) = &r.udp_stats {
        println!(
            "[SUM] jitter {:.3} ms, lost {}/{} ({:.2}%)",
            u.jitter_secs * 1000.0,
            u.lost,
            u.packets,
            u.lost_percent
        );
    }
    if let Some(connection) = &r.connection {
        match connection {
            #[cfg(feature = "iroh")]
            ConnectionInfo::Iroh(c) => match c.rtt_us {
                Some(rtt) => println!(
                    "[SUM] iroh path: {} (rtt {:.2} ms)",
                    c.observed_path,
                    rtt as f64 / 1000.0
                ),
                None => println!("[SUM] iroh path: {}", c.observed_path),
            },
            #[cfg(feature = "quic")]
            ConnectionInfo::Quic(c) => match c.rtt_us {
                Some(rtt) => println!(
                    "[SUM] quic handshake {:.2} ms (rtt {:.2} ms)",
                    c.handshake_ms,
                    rtt as f64 / 1000.0
                ),
                None => println!("[SUM] quic handshake {:.2} ms", c.handshake_ms),
            },
            #[cfg(feature = "webrtc")]
            ConnectionInfo::WebRtc(c) => println!(
                "[SUM] webrtc path: {} (setup {:.2} ms, {}→{} {})",
                c.path, c.setup_ms, c.local_candidate_type, c.remote_candidate_type, c.ice_protocol,
            ),
            #[cfg(not(any(feature = "iroh", feature = "quic", feature = "webrtc")))]
            ConnectionInfo::Unavailable => {}
        }
    }
}

/// iperf3-aligned JSON, matching `cli.ts`'s `toJson` so the Phase 3 matrix can
/// parse both implementations with one parser.
fn to_json(r: &TestResult, intervals: &[IntervalReport]) -> String {
    let mut end = serde_json::json!({
        "sum_sent": {
            "bytes": r.sent_bytes,
            "bits_per_second": r.send_bits_per_second,
            "seconds": r.duration_seconds,
        },
        "sum_received": {
            "bytes": r.received_bytes,
            "bits_per_second": r.receive_bits_per_second,
            "seconds": r.duration_seconds,
        },
    });
    if let Some(u) = &r.udp_stats {
        end["sum"] = serde_json::json!({
            "jitter_ms": u.jitter_secs * 1000.0,
            "lost_packets": u.lost,
            "packets": u.packets,
            "lost_percent": u.lost_percent,
        });
    }
    let mut value = serde_json::json!({
        "start": {
            "version": format!("netsu-rs-{}", netsu::VERSION),
            "test_start": {
                "protocol": if r.udp { "UDP" } else { "TCP" },
                "reverse": if r.reverse { 1 } else { 0 },
            },
        },
        "intervals": intervals.iter().map(|i| serde_json::json!({
            "sum": {
                "start": i.start,
                "end": i.end,
                "bytes": i.bytes,
                "bits_per_second": i.bits_per_second,
            }
        })).collect::<Vec<_>>(),
        "end": end,
    });
    // netsu extension: transport-specific connection diagnostics.
    if let Some(connection) = &r.connection {
        value["connection"] = connection_json(connection);
        value["connection"]["streams"] = serde_json::json!(r.local.streams.len());
    }
    serde_json::to_string(&value).unwrap_or_default()
}

/// Resolves on Ctrl-C or (on Unix) SIGTERM.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            // If SIGTERM can't be registered, Ctrl-C alone still works.
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netsu::error::SetupPhase;

    #[test]
    fn webrtc_setup_failures_have_stable_exit_codes_and_json_kinds() {
        let timeout = NetsuError::Setup {
            transport: "webrtc",
            phase: SetupPhase::SignalingConnect,
            detail: "signaling operation timed out".into(),
        };
        assert_eq!(runtime_exit_code(&timeout), EXIT_SETUP_TIMEOUT);
        let timeout_json: serde_json::Value =
            serde_json::from_str(&setup_error_json(&timeout).unwrap()).unwrap();
        assert_eq!(timeout_json["error"]["kind"], "setup_timeout");

        let direct = NetsuError::Setup {
            transport: "webrtc",
            phase: SetupPhase::IceConnected,
            detail: "direct path is unavailable".into(),
        };
        assert_eq!(runtime_exit_code(&direct), EXIT_DIRECT_UNAVAILABLE);
        let direct_json: serde_json::Value =
            serde_json::from_str(&setup_error_json(&direct).unwrap()).unwrap();
        assert_eq!(direct_json["error"]["kind"], "direct_path_unavailable");
        assert!(
            !setup_error_json(&direct)
                .unwrap()
                .contains("bits_per_second")
        );
    }
}
