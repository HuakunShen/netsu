//! `netsu mux` subcommands: the multiplexing/priority latency lab CLI. Builds a
//! `RunConfig` from flags and drives the runner/receiver. Only compiled with
//! `--features iroh`.

use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};

use netsu::mux::config::{
    FileMode, PriorityChangeConfig, PriorityConfig, RunConfig, ScenarioName, StreamSpec,
    WorkloadKind,
};
use netsu::mux::protocol::MUX_ALPN;
use netsu::mux::runner::MuxOutcome;
use netsu::mux::{receiver, runner};
use netsu::p2p::{addr, endpoint, rendezkey};

#[derive(Args)]
pub struct MuxArgs {
    #[command(subcommand)]
    cmd: MuxCmd,
}

#[derive(Subcommand)]
enum MuxCmd {
    /// Bind an iroh endpoint and serve mux runs (prints a code + ticket).
    Listen(ListenArgs),
    /// Run a scenario against a listener.
    Run(RunArgs),
    /// Run both ends in this process (smoke test).
    Local(ConfigArgs),
    /// Run the required-v1 experiment matrix (locally, or against a peer).
    Matrix(MatrixArgs),
}

#[derive(Args)]
struct MatrixArgs {
    /// Listener code or ticket. If omitted, runs each case locally in-process.
    #[arg(long)]
    peer: Option<String>,
    #[arg(long)]
    direct_only: bool,
    #[arg(long)]
    no_rendezkey: bool,
    #[arg(long)]
    rendezkey_url: Option<String>,
    #[arg(long, default_value = "required-v1")]
    profile: String,
    #[arg(long, default_value_t = 1)]
    repetitions: u32,
    #[arg(long, value_parser = parse_dur, default_value = "5s")]
    duration: Duration,
    #[arg(long, default_value_t = 12345)]
    seed: u64,
    /// Directory for per-case JSON + the comparison report.
    #[arg(long, default_value = "mux-matrix-out")]
    output_dir: std::path::PathBuf,
}

#[derive(Args)]
struct ListenArgs {
    #[arg(long)]
    direct_only: bool,
    #[arg(long)]
    no_rendezkey: bool,
    #[arg(long)]
    rendezkey_url: Option<String>,
}

#[derive(Args)]
struct RunArgs {
    /// Listener code or ticket.
    peer: String,
    #[arg(long)]
    direct_only: bool,
    #[arg(long)]
    no_rendezkey: bool,
    #[arg(long)]
    rendezkey_url: Option<String>,
    #[command(flatten)]
    config: ConfigArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum ScenarioArg {
    InputOnly,
    ClipboardOnly,
    FileOnly,
    InputFile,
    Mixed,
    Custom,
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetArg {
    Equal,
    Graded,
    Inverted,
}

#[derive(Clone, Copy, ValueEnum)]
enum FileModeArg {
    Saturating,
    FixedRate,
}

fn parse_dur(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| e.to_string())
}

#[derive(Args)]
struct ConfigArgs {
    #[arg(long, value_enum, default_value = "mixed")]
    scenario: ScenarioArg,
    #[arg(long, value_parser = parse_dur, default_value = "15s")]
    duration: Duration,
    #[arg(long, value_parser = parse_dur, default_value = "2s")]
    warmup: Duration,
    #[arg(long, value_parser = parse_dur, default_value = "1s")]
    cooldown: Duration,
    #[arg(long, default_value_t = 12345)]
    seed: u64,
    #[arg(long, value_enum, default_value = "graded")]
    priorities: PresetArg,
    #[arg(long)]
    input_priority: Option<i32>,
    #[arg(long)]
    file_priority: Option<i32>,
    #[arg(long)]
    cast_priority: Option<i32>,
    #[arg(long, default_value_t = 125)]
    input_hz: u32,
    #[arg(long, default_value_t = 64)]
    input_payload: usize,
    #[arg(long, value_enum, default_value = "saturating")]
    file_mode: FileModeArg,
    #[arg(long)]
    file_rate_mbps: Option<f64>,
    #[arg(long, default_value_t = 1)]
    file_streams: usize,
    #[arg(long, default_value_t = 20.0)]
    cast_bitrate_mbps: f64,
    #[arg(long, default_value_t = 1)]
    cast_streams: usize,
    /// A custom stream (repeatable): `prio=30,hz=125,deadline=100ms` etc.
    #[arg(long = "stream")]
    streams: Vec<String>,
    #[arg(long, value_parser = parse_dur)]
    priority_change_after: Option<Duration>,
    #[arg(long)]
    priority_change_to: Option<i32>,
    #[arg(long, default_value = "file")]
    priority_change_workload: String,
    #[arg(long)]
    direct_only: bool,
    #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
    send_fairness: bool,
    /// Emit the result as JSON to stdout instead of a human summary.
    #[arg(long)]
    json: bool,
    /// Write the full schema-v1 result JSON to this file.
    #[arg(long)]
    json_out: Option<std::path::PathBuf>,
    /// Write per-message RTT samples as NDJSON to this file.
    #[arg(long)]
    samples_out: Option<std::path::PathBuf>,
}

impl ConfigArgs {
    fn build(&self) -> Result<RunConfig, String> {
        let mut cfg = RunConfig {
            scenario: match self.scenario {
                ScenarioArg::InputOnly => ScenarioName::InputOnly,
                ScenarioArg::ClipboardOnly => ScenarioName::ClipboardOnly,
                ScenarioArg::FileOnly => ScenarioName::FileOnly,
                ScenarioArg::InputFile => ScenarioName::InputFile,
                ScenarioArg::Mixed => ScenarioName::Mixed,
                ScenarioArg::Custom => ScenarioName::Custom,
            },
            duration: self.duration,
            warmup: self.warmup,
            cooldown: self.cooldown,
            seed: self.seed,
            priorities: match self.priorities {
                PresetArg::Equal => PriorityConfig::equal(),
                PresetArg::Graded => PriorityConfig::graded(),
                PresetArg::Inverted => PriorityConfig::inverted(),
            },
            ..Default::default()
        };
        if let Some(p) = self.input_priority {
            cfg.priorities.input = p;
        }
        if let Some(p) = self.file_priority {
            cfg.priorities.file = p;
        }
        if let Some(p) = self.cast_priority {
            cfg.priorities.cast = p;
        }
        cfg.input.frequency_hz = self.input_hz;
        cfg.input.payload_bytes = self.input_payload;
        cfg.file.mode = match self.file_mode {
            FileModeArg::Saturating => FileMode::Saturating,
            FileModeArg::FixedRate => FileMode::FixedRate,
        };
        cfg.file.rate_mbps = self.file_rate_mbps;
        cfg.file.streams = self.file_streams;
        cfg.cast.bitrate_mbps = self.cast_bitrate_mbps;
        cfg.cast.streams = self.cast_streams;
        cfg.transport.direct_only = self.direct_only;
        cfg.transport.send_fairness = self.send_fairness;

        for spec in &self.streams {
            cfg.custom_streams
                .push(StreamSpec::parse(spec).map_err(|e| format!("--stream '{spec}': {e:#}"))?);
        }

        if let (Some(after), Some(to)) = (self.priority_change_after, self.priority_change_to) {
            let workload = match self.priority_change_workload.as_str() {
                "input" => WorkloadKind::Input,
                "clipboard" => WorkloadKind::Clipboard,
                "cast" => WorkloadKind::Cast,
                "file" => WorkloadKind::File,
                "custom" => WorkloadKind::Custom,
                other => return Err(format!("unknown priority-change workload: {other}")),
            };
            cfg.priority_change = Some(PriorityChangeConfig { after, workload, new_priority: to });
        }

        cfg.validate().map_err(|e| format!("{e:#}"))?;
        Ok(cfg)
    }
}

/// Entry point for `netsu mux`.
pub async fn run(args: MuxArgs) -> i32 {
    let result = match args.cmd {
        MuxCmd::Listen(a) => run_listen(a).await,
        MuxCmd::Run(a) => run_remote(a).await,
        MuxCmd::Local(a) => run_local(a).await,
        MuxCmd::Matrix(a) => run_matrix(a).await,
    };
    match result {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("netsu mux: {msg}");
            1
        }
    }
}

async fn run_listen(a: ListenArgs) -> Result<(), String> {
    let (endpoint, ticket) =
        endpoint::bind_listener_with_ticket(MUX_ALPN, a.direct_only, true)
            .await
            .map_err(|e| format!("{e:#}"))?;
    println!("netsu mux listening (iroh)");
    if !a.no_rendezkey {
        let url = a.rendezkey_url.as_deref().unwrap_or(rendezkey::DEFAULT_BASE_URL);
        if let Some(token) = rendezkey::token_from_env() {
            match rendezkey::store(url, &token, &ticket, 3600, 10).await {
                Ok(code) => println!("code:   {code}   (share this — expires in 60m)"),
                Err(e) => eprintln!("(rendez-key unavailable: {e:#}; share the ticket)"),
            }
        }
    }
    println!("ticket: {ticket}");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            accepted = endpoint.accept() => {
                let Some(incoming) = accepted else { break };
                tokio::spawn(async move {
                    if let Ok(conn) = incoming.await
                        && let Err(e) = receiver::serve(conn).await
                    {
                        eprintln!("netsu mux: session ended: {e:#}");
                    }
                });
            }
        }
    }
    endpoint.close().await;
    Ok(())
}

async fn run_remote(a: RunArgs) -> Result<(), String> {
    let config = a.config.build()?;
    let url = a.rendezkey_url.as_deref().unwrap_or(rendezkey::DEFAULT_BASE_URL);
    let ticket = if a.no_rendezkey {
        a.peer.clone()
    } else {
        addr::resolve_ticket(&a.peer, url).await.map_err(|e| format!("{e:#}"))?
    };
    let peer = endpoint::parse_ticket(&ticket).map_err(|e| format!("{e:#}"))?;
    let ep = endpoint::bind_client(a.direct_only, config.transport.send_fairness)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let connection = endpoint::connect(&ep, peer, MUX_ALPN).await.map_err(|e| format!("{e:#}"))?;

    let outcome = runner::run(&connection, &config).await.map_err(|e| format!("{e:#}"))?;
    connection.close(0u32.into(), b"done");
    ep.close().await;
    finish(&outcome, &a.config)
}

async fn run_local(a: ConfigArgs) -> Result<(), String> {
    let config = a.build()?;
    let pair = endpoint::LocalPair::connect(MUX_ALPN).await.map_err(|e| format!("{e:#}"))?;
    let server_conn = pair.server_connection.clone();
    let serve = tokio::spawn(async move { receiver::serve(server_conn).await });
    let outcome = runner::run(&pair.client_connection, &config).await.map_err(|e| format!("{e:#}"))?;
    let _ = serve.await;
    pair.close().await;
    finish(&outcome, &a)
}

async fn run_matrix(a: MatrixArgs) -> Result<(), String> {
    std::fs::create_dir_all(&a.output_dir).map_err(|e| format!("create output dir: {e}"))?;
    let base = RunConfig {
        duration: a.duration,
        // Scale warmup/cooldown to the (possibly short) matrix duration.
        warmup: a.duration / 5,
        cooldown: a.duration / 10,
        seed: a.seed,
        transport: netsu::mux::config::TransportConfig { send_fairness: true, direct_only: a.direct_only },
        ..Default::default()
    };
    let cases = netsu::mux::matrix::required_v1(&base);

    let peer_ticket = match &a.peer {
        Some(p) => {
            let url = a.rendezkey_url.as_deref().unwrap_or(rendezkey::DEFAULT_BASE_URL);
            Some(if a.no_rendezkey {
                p.clone()
            } else {
                addr::resolve_ticket(p, url).await.map_err(|e| format!("{e:#}"))?
            })
        }
        None => None,
    };

    let mut results = Vec::new();
    for case in &cases {
        for rep in 0..a.repetitions {
            let mut cfg = case.config.clone();
            cfg.seed = a.seed + rep as u64;
            let outcome = run_one(peer_ticket.as_deref(), a.direct_only, &cfg)
                .await
                .map_err(|e| format!("case {}: {e}", case.name))?;
            let result = netsu::mux::result::MuxResult::from_outcome(&outcome, cfg.seed);
            let path = a.output_dir.join(format!("{}-{:02}.json", case.name, rep));
            netsu::mux::output::write_json_atomic(&path, &result).map_err(|e| format!("{e:#}"))?;
            println!(
                "  {:<24} rep {rep}  p99 {:>6} us  {:>7.1} Mbps",
                case.name,
                result.aggregate.probe_p99_us.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                result.aggregate.total_throughput_mbps
            );
            results.push((case.name.clone(), result));
        }
    }

    let report = netsu::mux::matrix::aggregate(&a.profile, &results);
    let report_path = a.output_dir.join("comparison.json");
    netsu::mux::output::write_json_atomic(&report_path, &report).map_err(|e| format!("{e:#}"))?;
    println!(
        "load-induced input p99 delta: {} us",
        report.load_induced_input_p99_delta_us.map(|v| v.to_string()).unwrap_or_else(|| "n/a".into())
    );
    println!("wrote {}", report_path.display());
    Ok(())
}

async fn run_one(
    peer_ticket: Option<&str>,
    direct_only: bool,
    config: &RunConfig,
) -> Result<MuxOutcome, String> {
    match peer_ticket {
        Some(ticket) => {
            let peer = endpoint::parse_ticket(ticket).map_err(|e| format!("{e:#}"))?;
            let ep = endpoint::bind_client(direct_only, config.transport.send_fairness)
                .await
                .map_err(|e| format!("{e:#}"))?;
            let conn = endpoint::connect(&ep, peer, MUX_ALPN).await.map_err(|e| format!("{e:#}"))?;
            let outcome = runner::run(&conn, config).await.map_err(|e| format!("{e:#}"))?;
            conn.close(0u32.into(), b"done");
            ep.close().await;
            Ok(outcome)
        }
        None => {
            let pair = endpoint::LocalPair::connect(MUX_ALPN).await.map_err(|e| format!("{e:#}"))?;
            let server_conn = pair.server_connection.clone();
            let serve = tokio::spawn(async move { receiver::serve(server_conn).await });
            let outcome = runner::run(&pair.client_connection, config).await.map_err(|e| format!("{e:#}"))?;
            let _ = serve.await;
            pair.close().await;
            Ok(outcome)
        }
    }
}

/// Write `--json-out` (if set) and print the human/JSON summary to stdout.
fn finish(outcome: &MuxOutcome, cfg: &ConfigArgs) -> Result<(), String> {
    if let Some(path) = &cfg.json_out {
        let result = netsu::mux::result::MuxResult::from_outcome(outcome, cfg.seed);
        netsu::mux::output::write_json_atomic(path, &result).map_err(|e| format!("{e:#}"))?;
        eprintln!("netsu mux: wrote {}", path.display());
    }
    if let Some(path) = &cfg.samples_out {
        let mut ndjson = String::new();
        for s in &outcome.streams {
            for (elapsed_us, rtt_us) in &s.rtt_samples {
                ndjson.push_str(&format!(
                    "{{\"index\":{},\"elapsedUs\":{},\"rttUs\":{}}}\n",
                    s.index, elapsed_us, rtt_us
                ));
            }
        }
        netsu::mux::output::write_atomic(path, ndjson.as_bytes()).map_err(|e| format!("{e:#}"))?;
        eprintln!("netsu mux: wrote {}", path.display());
    }
    report(outcome, cfg.json);
    Ok(())
}

fn report(outcome: &MuxOutcome, json: bool) {
    if json {
        println!("{}", mux_outcome_json(outcome));
        return;
    }
    println!("mux run {} — {:?}", outcome.run_id, outcome.scenario);
    for s in &outcome.streams {
        let dur = outcome.duration.as_secs_f64();
        let mbps = s.bytes_sent as f64 * 8.0 / 1e6 / dur;
        match &s.latency {
            Some(l) => println!(
                "  [{:?}#{} prio {}] {:>7.1} Mbps  p50 {:.2}ms p99 {:.2}ms  miss {:.1}%",
                s.kind,
                s.index,
                s.priority,
                mbps,
                l.p50_us as f64 / 1000.0,
                l.p99_us as f64 / 1000.0,
                l.deadline_exceeded_rate * 100.0
            ),
            None => println!(
                "  [{:?}#{} prio {}] {:>7.1} Mbps  (load)",
                s.kind, s.index, s.priority, mbps
            ),
        }
    }
}

/// Minimal outcome JSON. Phase 5 replaces this with a schema'd `MuxResult`.
fn mux_outcome_json(outcome: &MuxOutcome) -> String {
    let dur = outcome.duration.as_secs_f64();
    let streams: Vec<_> = outcome
        .streams
        .iter()
        .map(|s| {
            let mbps = s.bytes_sent as f64 * 8.0 / 1e6 / dur;
            serde_json::json!({
                "kind": format!("{:?}", s.kind),
                "index": s.index,
                "priority": s.priority,
                "measured": s.measured,
                "bytes_sent": s.bytes_sent,
                "bytes_received": s.bytes_received,
                "mbps": mbps,
                "latency": s.latency.as_ref().map(|l| serde_json::json!({
                    "count": l.count,
                    "p50_us": l.p50_us,
                    "p99_us": l.p99_us,
                    "p999_us": l.p999_us,
                    "max_us": l.max_us,
                    "deadline_exceeded_rate": l.deadline_exceeded_rate,
                })),
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({
        "run_id": outcome.run_id.to_string(),
        "scenario": format!("{:?}", outcome.scenario),
        "duration_s": dur,
        "streams": streams,
    }))
    .unwrap_or_default()
}
