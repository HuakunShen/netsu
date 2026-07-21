//! Two-device keyboard/mouse sharing demo (perceived-latency tool). A separate
//! example — never part of the default `netsu` binary. Requires `input-demo`.
//!
//!   # on the controlled device (receives input):
//!   cargo run --example kbm-demo --features input-demo -- controlled --inject-input
//!   # on the controller device (sends its input), using the printed code/ticket:
//!   cargo run --example kbm-demo --features input-demo -- controller <CODE|TICKET> --bulk-streams 2
//!
//! Safety: injection is opt-in (`--inject-input`); the controller stops on `q`
//! or Escape+Ctrl+Alt; held keys are always released on stop/disconnect.

use std::time::Duration;

use clap::{Parser, Subcommand};
use netsu::demo::session::{ControlledConfig, ControllerConfig, run_controlled, run_controller};

#[derive(Parser)]
#[command(name = "kbm-demo", about = "keyboard/mouse sharing over iroh (perceived-latency demo)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Receive and (optionally) inject a controller's input.
    Controlled(ControlledArgs),
    /// Capture this device's input and stream it to a controlled peer.
    Controller(ControllerArgs),
}

#[derive(Parser)]
struct ControlledArgs {
    /// Actually inject received input (off by default for safety).
    #[arg(long)]
    inject_input: bool,
    /// Only accept this endpoint id.
    #[arg(long)]
    allow_peer: Option<String>,
    /// Reject input older than this (also the replay window).
    #[arg(long, value_parser = parse_dur, default_value = "3s")]
    idle_timeout: Duration,
    #[arg(long)]
    direct_only: bool,
    #[arg(long)]
    no_rendezkey: bool,
    #[arg(long)]
    rendezkey_url: Option<String>,
}

#[derive(Parser)]
struct ControllerArgs {
    /// Controlled peer's rendez-key code or ticket.
    peer: String,
    #[arg(long, value_parser = parse_dur, default_value = "30s")]
    duration: Duration,
    #[arg(long, default_value_t = 1)]
    bulk_streams: u16,
    /// Aggregate bulk-load cap in Mbps (unset = saturating).
    #[arg(long)]
    bulk_rate_mbps: Option<f64>,
    #[arg(long, default_value_t = 4096)]
    hook_capacity: usize,
    #[arg(long)]
    direct_only: bool,
    #[arg(long)]
    no_rendezkey: bool,
    #[arg(long)]
    rendezkey_url: Option<String>,
}

fn parse_dur(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Controlled(a) => {
            run_controlled(ControlledConfig {
                allow_peer: a.allow_peer,
                inject_input: a.inject_input,
                idle_timeout: a.idle_timeout,
                direct_only: a.direct_only,
                no_rendezkey: a.no_rendezkey,
                rendezkey_url: a.rendezkey_url,
            })
            .await
        }
        Cmd::Controller(a) => {
            run_controller(ControllerConfig {
                peer: a.peer,
                duration: a.duration,
                bulk_streams: a.bulk_streams,
                bulk_rate_mbps: a.bulk_rate_mbps,
                hook_capacity: a.hook_capacity,
                direct_only: a.direct_only,
                no_rendezkey: a.no_rendezkey,
                rendezkey_url: a.rendezkey_url,
            })
            .await
        }
    };
    if let Err(e) = result {
        eprintln!("kbm-demo: {e:#}");
        std::process::exit(1);
    }
}
