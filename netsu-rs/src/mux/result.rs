//! The serialized, schema'd result of a mux run (`schema/mux-result-v1.json`).
//! Throughput and latency are measured-window figures.

use schemars::JsonSchema;
use serde::Serialize;

use crate::mux::metrics::jains_fairness;
use crate::mux::resources::ResourceSummary;
use crate::mux::runner::MuxOutcome;

pub const MUX_SCHEMA_VERSION: u32 = 1;
pub const MUX_TOOL: &str = "netsu-mux";

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LatencyResult {
    pub count: u64,
    pub timeout_count: u64,
    pub min_us: u64,
    pub mean_us: f64,
    pub p50_us: u64,
    pub p90_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
    pub deadline_exceeded: u64,
    pub deadline_exceeded_rate: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamResult {
    pub kind: String,
    pub index: u16,
    pub priority: i32,
    pub measured: bool,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub throughput_mbps: f64,
    pub latency: Option<LatencyResult>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
    pub total_throughput_mbps: f64,
    /// The lowest-index measured probe's p99 RTT (the headline latency), if any.
    pub probe_p99_us: Option<u64>,
    /// Jain's fairness over the load streams' throughputs.
    pub jain_fairness: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MuxResult {
    pub schema_version: u32,
    pub tool: String,
    pub iroh_version: String,
    pub run_id: String,
    pub scenario: String,
    pub seed: u64,
    pub duration_ms: u64,
    pub measure_window_ms: u64,
    pub streams: Vec<StreamResult>,
    pub aggregate: Aggregate,
    pub resources: Option<ResourceSummary>,
}

impl MuxResult {
    pub fn from_outcome(outcome: &MuxOutcome, seed: u64) -> Self {
        let window_s = outcome.measure_window.as_secs_f64().max(f64::MIN_POSITIVE);
        let streams: Vec<StreamResult> = outcome
            .streams
            .iter()
            .map(|s| StreamResult {
                kind: format!("{:?}", s.kind),
                index: s.index,
                priority: s.priority,
                measured: s.measured,
                bytes_sent: s.bytes_sent,
                bytes_received: s.bytes_received,
                throughput_mbps: s.bytes_received as f64 * 8.0 / 1e6 / window_s,
                latency: s.latency.as_ref().map(|l| LatencyResult {
                    count: l.count,
                    timeout_count: l.timeout_count,
                    min_us: l.min_us,
                    mean_us: l.mean_us,
                    p50_us: l.p50_us,
                    p90_us: l.p90_us,
                    p99_us: l.p99_us,
                    p999_us: l.p999_us,
                    max_us: l.max_us,
                    deadline_exceeded: l.deadline_exceeded,
                    deadline_exceeded_rate: l.deadline_exceeded_rate,
                }),
            })
            .collect();

        let total_throughput_mbps = streams.iter().map(|s| s.throughput_mbps).sum();
        let load: Vec<f64> = streams
            .iter()
            .filter(|s| !s.measured)
            .map(|s| s.throughput_mbps)
            .collect();
        let probe_p99_us = outcome
            .streams
            .iter()
            .filter(|s| s.measured)
            .filter_map(|s| s.latency.as_ref().map(|l| (s.index, l.p99_us)))
            .min_by_key(|(idx, _)| *idx)
            .map(|(_, p99)| p99);

        MuxResult {
            schema_version: MUX_SCHEMA_VERSION,
            tool: MUX_TOOL.to_string(),
            iroh_version: "1.0.2".to_string(),
            run_id: outcome.run_id.to_string(),
            scenario: format!("{:?}", outcome.scenario),
            seed,
            duration_ms: outcome.duration.as_millis() as u64,
            measure_window_ms: outcome.measure_window.as_millis() as u64,
            streams,
            aggregate: Aggregate {
                total_throughput_mbps,
                probe_p99_us,
                jain_fairness: jains_fairness(&load),
            },
            resources: outcome.resources.clone(),
        }
    }
}
