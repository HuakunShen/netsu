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
    /// Emit the result as JSON instead of a human summary.
    #[arg(long)]
    json: bool,
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
    report(&outcome, a.config.json);
    Ok(())
}

async fn run_local(a: ConfigArgs) -> Result<(), String> {
    let config = a.build()?;
    let pair = endpoint::LocalPair::connect(MUX_ALPN).await.map_err(|e| format!("{e:#}"))?;
    let server_conn = pair.server_connection.clone();
    let serve = tokio::spawn(async move { receiver::serve(server_conn).await });
    let outcome = runner::run(&pair.client_connection, &config).await.map_err(|e| format!("{e:#}"))?;
    let _ = serve.await;
    pair.close().await;
    report(&outcome, a.json);
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
