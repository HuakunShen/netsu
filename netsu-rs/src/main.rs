//! netsu CLI — iperf3-style flags over the library. Kept thin: argument
//! parsing, validation, output formatting, and process lifecycle only; all
//! measurement logic lives in the `netsu` library.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, Subcommand};

use netsu::client::{ClientOptions, TestResult, Transport, run_client};
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
    /// iroh only: bind a direct-only endpoint (no relay/discovery).
    #[arg(long)]
    direct_only: bool,
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
    /// iroh only: require a direct path (fail if the connection uses a relay).
    #[arg(long)]
    direct_only: bool,
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

/// Resolve `--ws` / `--iroh` against compiled-in features. Both stay valid
/// flags even without their feature so the error is actionable rather than a
/// clap "unknown argument".
fn select_transport(ws: bool, iroh: bool) -> Result<Transport, String> {
    match (ws, iroh) {
        (true, true) => Err("--ws and --iroh are mutually exclusive".to_string()),
        (true, false) => {
            #[cfg(feature = "ws")]
            {
                Ok(Transport::Ws)
            }
            #[cfg(not(feature = "ws"))]
            {
                Err("ws support not compiled in; rebuild with --features ws".to_string())
            }
        }
        (false, true) => {
            #[cfg(feature = "iroh")]
            {
                Ok(Transport::Iroh)
            }
            #[cfg(not(feature = "iroh"))]
            {
                Err("iroh support not compiled in; rebuild with --features iroh".to_string())
            }
        }
        (false, false) => Ok(Transport::Tcp),
    }
}

async fn run_server(a: ServerArgs) -> i32 {
    let transport = match select_transport(a.ws, a.iroh) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("netsu server: {e}");
            return 1;
        }
    };
    let server = match start_server(ServerOptions {
        port: a.port,
        transport,
        direct_only: a.direct_only,
        ..Default::default()
    })
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("netsu server: {}", describe(&e));
            return 1;
        }
    };
    match &server.endpoint_ticket {
        // iroh: the client dials this ticket via `--peer`/positional HOST.
        Some(ticket) => {
            println!("netsu server listening (iroh)");
            println!("ticket: {ticket}");
        }
        None => println!(
            "netsu server listening on {} ({})",
            server.port,
            if a.ws { "ws" } else { "tcp" }
        ),
    }
    // The listening server holds the runtime open; wait for Ctrl-C/SIGTERM,
    // then release the port cleanly instead of being killed out from under it.
    wait_for_shutdown().await;
    server.close().await;
    0
}

async fn run_client_cmd(a: ClientArgs) -> i32 {
    match run_client_inner(a).await {
        Ok(()) => 0,
        Err(msg) => {
            // Always to stderr, so --json's stdout stays pure even on failure.
            eprintln!("netsu client: {msg}");
            1
        }
    }
}

async fn run_client_inner(a: ClientArgs) -> Result<(), String> {
    let transport = select_transport(a.ws, a.iroh)?;
    if a.udp && a.ws {
        return Err("--udp and --ws are mutually exclusive".to_string());
    }
    if a.udp && a.iroh {
        return Err("--udp and --iroh are mutually exclusive (iroh is reliable)".to_string());
    }
    if a.time < 1 {
        return Err(format!("invalid time: {} (must be >= 1)", a.time));
    }
    if a.parallel < 1 {
        return Err(format!("invalid parallel: {} (must be >= 1)", a.parallel));
    }
    let bandwidth = match a.bandwidth.as_deref() {
        Some(s) => Some(parse_bandwidth(s).map_err(|e| e.to_string())?),
        None => None,
    };
    let len = match a.len.as_deref() {
        Some(s) => Some(parse_len(s).map_err(|e| e.to_string())?),
        None => None,
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
    };

    let result = run_client(&a.host, opts, Some(on_interval))
        .await
        .map_err(|e| describe(&e))?;

    if a.json {
        let intervals = intervals.lock().map_err(|_| "interval lock poisoned")?;
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
    let value = serde_json::json!({
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
