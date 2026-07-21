//! What a mux run does: the scenario (which traffic kinds), each kind's
//! priority/rate/payload, and the resolution of all that into a flat list of
//! streams the runner opens. A `custom` scenario replaces the fixed kinds with
//! an explicit list of `--stream` specs.

use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};

/// Which mix of traffic to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScenarioName {
    InputOnly,
    ClipboardOnly,
    FileOnly,
    InputFile,
    Mixed,
    /// An explicit list of streams (see [`RunConfig::custom_streams`]).
    Custom,
}

/// A type of traffic. `Control`/`Ack` are internal (handshake + latency echo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkloadKind {
    Control,
    Ack,
    Input,
    Clipboard,
    Cast,
    File,
    /// A user-defined stream from the `custom` scenario.
    Custom,
}

/// How a stream's payload is paced.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Pacing {
    /// As fast as the link allows (a load generator).
    Saturating,
    /// A fixed bitrate in megabits/second.
    RateMbps(f64),
    /// A fixed frequency in Hz (one payload per 1/hz seconds — small probes).
    Hz(u32),
}

/// Per-workload QUIC stream priorities (higher = scheduled first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriorityConfig {
    pub ack: i32,
    pub input: i32,
    pub clipboard: i32,
    pub cast: i32,
    pub file: i32,
}

impl PriorityConfig {
    pub const fn equal() -> Self {
        Self {
            ack: 0,
            input: 0,
            clipboard: 0,
            cast: 0,
            file: 0,
        }
    }
    pub const fn graded() -> Self {
        Self {
            ack: 40,
            input: 30,
            clipboard: 20,
            cast: 10,
            file: 0,
        }
    }
    pub const fn inverted() -> Self {
        Self {
            ack: 40,
            input: 0,
            clipboard: 10,
            cast: 20,
            file: 30,
        }
    }
    pub const fn for_kind(&self, kind: WorkloadKind) -> i32 {
        match kind {
            WorkloadKind::Control | WorkloadKind::Ack => self.ack,
            WorkloadKind::Input => self.input,
            WorkloadKind::Clipboard => self.clipboard,
            WorkloadKind::Cast => self.cast,
            WorkloadKind::File => self.file,
            WorkloadKind::Custom => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputConfig {
    pub payload_bytes: usize,
    pub frequency_hz: u32,
    pub deadline: Duration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardConfig {
    pub payload_sizes: Vec<usize>,
    pub interval_min: Duration,
    pub interval_max: Duration,
    pub deadline: Duration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CastConfig {
    pub bitrate_mbps: f64,
    pub chunk_bytes: usize,
    pub streams: usize,
    pub pacing_interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileMode {
    Saturating,
    FixedRate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileConfig {
    pub mode: FileMode,
    pub rate_mbps: Option<f64>,
    pub chunk_bytes: usize,
    pub streams: usize,
}

/// One user-defined stream in the `custom` scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSpec {
    pub priority: i32,
    pub pacing: Pacing,
    pub payload_bytes: usize,
    pub chunk_bytes: usize,
    /// Presence marks this a measured latency probe (ACK-RTT + deadline).
    pub deadline: Option<Duration>,
    pub count: u16,
}

impl StreamSpec {
    /// True if this stream is a measured latency probe rather than pure load.
    pub fn measured(&self) -> bool {
        self.deadline.is_some()
    }

    /// Parse the CLI grammar, e.g. `prio=30,hz=125,payload=64,deadline=100ms`
    /// or `prio=0,rate=800mbps,chunk=65536,count=2` or `prio=0,saturating`.
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        let mut priority: Option<i32> = None;
        let mut rate: Option<f64> = None;
        let mut hz: Option<u32> = None;
        let mut saturating = false;
        let mut payload: Option<usize> = None;
        let mut chunk: Option<usize> = None;
        let mut deadline: Option<Duration> = None;
        let mut role_probe: Option<bool> = None;
        let mut count: u16 = 1;

        for field in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, val) = match field.split_once('=') {
                Some((k, v)) => (k.trim(), Some(v.trim())),
                None => (field, None),
            };
            match key {
                "prio" | "priority" => {
                    priority = Some(req(val, key)?.parse().context("prio must be an integer")?)
                }
                "rate" => rate = Some(parse_rate_mbps(req(val, key)?)?),
                "saturating" => saturating = true,
                "hz" => hz = Some(req(val, key)?.parse().context("hz must be an integer")?),
                "payload" => {
                    payload = Some(req(val, key)?.parse().context("payload must be bytes")?)
                }
                "chunk" => chunk = Some(req(val, key)?.parse().context("chunk must be bytes")?),
                "deadline" => {
                    deadline = Some(
                        humantime::parse_duration(req(val, key)?)
                            .context("deadline must be a duration, e.g. 100ms")?,
                    )
                }
                "role" => {
                    role_probe = Some(match req(val, key)? {
                        "probe" => true,
                        "load" => false,
                        other => bail!("role must be probe|load, got {other}"),
                    })
                }
                "count" => count = req(val, key)?.parse().context("count must be an integer")?,
                other => bail!("unknown --stream field: {other}"),
            }
        }

        let priority = priority.context("--stream requires prio=<i32>")?;
        // Exactly one pacing source.
        let pacing = match (saturating, rate, hz) {
            (true, None, None) => Pacing::Saturating,
            (false, Some(r), None) => Pacing::RateMbps(r),
            (false, None, Some(h)) => Pacing::Hz(h),
            (false, None, None) => bail!("--stream needs one of rate=, hz=, or saturating"),
            _ => bail!("--stream: rate/hz/saturating are mutually exclusive"),
        };
        // A probe by explicit role, or implied by a deadline.
        let is_probe = role_probe.unwrap_or_else(|| deadline.is_some());
        let deadline = match (is_probe, deadline) {
            (true, Some(d)) => Some(d),
            (true, None) => Some(Duration::from_millis(100)), // default probe deadline
            (false, _) => None,
        };
        let payload = payload.unwrap_or(if is_probe { 64 } else { 64 * 1024 });
        let chunk = chunk.unwrap_or(payload);
        ensure!(count >= 1, "--stream count must be >= 1");
        ensure!(payload >= 1, "--stream payload must be >= 1 byte");
        ensure!(chunk >= 1, "--stream chunk must be >= 1 byte");

        Ok(StreamSpec {
            priority,
            pacing,
            payload_bytes: payload,
            chunk_bytes: chunk,
            deadline,
            count,
        })
    }
}

fn req<'a>(val: Option<&'a str>, key: &str) -> anyhow::Result<&'a str> {
    val.with_context(|| format!("--stream field {key} needs a value"))
}

/// Parse a `<n>mbps` (or bare `<n>`) rate into megabits/second.
fn parse_rate_mbps(s: &str) -> anyhow::Result<f64> {
    let n = s.trim_end_matches("mbps").trim_end_matches("Mbps").trim();
    let v: f64 = n.parse().context("rate must be a number of Mbps")?;
    ensure!(v.is_finite() && v > 0.0, "rate must be positive");
    Ok(v)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportConfig {
    pub send_fairness: bool,
    pub direct_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriorityChangeConfig {
    pub after: Duration,
    pub workload: WorkloadKind,
    pub new_priority: i32,
}

/// A fully resolved stream the runner opens: one entry per actual QUIC stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStream {
    pub kind: WorkloadKind,
    pub index: u16,
    pub priority: i32,
    pub pacing: Pacing,
    pub payload_bytes: usize,
    pub chunk_bytes: usize,
    pub deadline: Option<Duration>,
    pub measured: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunConfig {
    pub scenario: ScenarioName,
    pub duration: Duration,
    pub warmup: Duration,
    pub cooldown: Duration,
    pub seed: u64,
    pub ack_timeout: Duration,
    pub priorities: PriorityConfig,
    pub input: InputConfig,
    pub clipboard: ClipboardConfig,
    pub cast: CastConfig,
    pub file: FileConfig,
    pub custom_streams: Vec<StreamSpec>,
    pub transport: TransportConfig,
    pub priority_change: Option<PriorityChangeConfig>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            scenario: ScenarioName::Mixed,
            duration: Duration::from_secs(15),
            warmup: Duration::from_secs(2),
            cooldown: Duration::from_secs(1),
            seed: 12_345,
            ack_timeout: Duration::from_secs(2),
            priorities: PriorityConfig::graded(),
            input: InputConfig {
                payload_bytes: 64,
                frequency_hz: 125,
                deadline: Duration::from_millis(100),
            },
            clipboard: ClipboardConfig {
                payload_sizes: vec![1024, 16 * 1024, 64 * 1024],
                interval_min: Duration::from_millis(250),
                interval_max: Duration::from_secs(1),
                deadline: Duration::from_secs(1),
            },
            cast: CastConfig {
                bitrate_mbps: 20.0,
                chunk_bytes: 16 * 1024,
                streams: 1,
                pacing_interval: Duration::from_millis(5),
            },
            file: FileConfig {
                mode: FileMode::Saturating,
                rate_mbps: None,
                chunk_bytes: 64 * 1024,
                streams: 1,
            },
            custom_streams: Vec::new(),
            transport: TransportConfig {
                send_fairness: true,
                direct_only: false,
            },
            priority_change: None,
        }
    }
}

impl RunConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(!self.duration.is_zero(), "duration must be > 0");
        ensure!(!self.ack_timeout.is_zero(), "ack timeout must be > 0");
        ensure!(
            self.warmup + self.cooldown < self.duration,
            "warmup + cooldown must be shorter than duration"
        );
        if self.scenario == ScenarioName::Custom {
            ensure!(
                !self.custom_streams.is_empty(),
                "custom scenario requires at least one --stream"
            );
        }
        if self.file.mode == FileMode::FixedRate {
            ensure!(
                matches!(self.file.rate_mbps, Some(r) if r > 0.0),
                "fixed-rate file mode requires a positive --file-rate-mbps"
            );
        }
        if let Some(change) = &self.priority_change {
            ensure!(
                change.after < self.duration,
                "priority change must occur before the run ends"
            );
        }
        Ok(())
    }

    /// True if `kind` participates in this scenario.
    pub fn has_workload(&self, kind: WorkloadKind) -> bool {
        match self.scenario {
            ScenarioName::InputOnly => kind == WorkloadKind::Input,
            ScenarioName::ClipboardOnly => kind == WorkloadKind::Clipboard,
            ScenarioName::FileOnly => kind == WorkloadKind::File,
            ScenarioName::InputFile => matches!(kind, WorkloadKind::Input | WorkloadKind::File),
            ScenarioName::Mixed => matches!(
                kind,
                WorkloadKind::Input
                    | WorkloadKind::Clipboard
                    | WorkloadKind::Cast
                    | WorkloadKind::File
            ),
            ScenarioName::Custom => kind == WorkloadKind::Custom,
        }
    }

    /// Flatten the scenario + per-kind configs (or the custom specs) into the
    /// concrete list of streams to open. Data-stream indices are assigned
    /// densely; the control and ACK streams are implicit and not listed here.
    pub fn resolve_streams(&self) -> Vec<ResolvedStream> {
        let mut out = Vec::new();
        let mut next_index: u16 = 0;
        let push =
            |kind, priority, pacing, payload, chunk, deadline, out: &mut Vec<_>, idx: &mut u16| {
                out.push(ResolvedStream {
                    kind,
                    index: *idx,
                    priority,
                    pacing,
                    payload_bytes: payload,
                    chunk_bytes: chunk,
                    deadline,
                    measured: deadline.is_some(),
                });
                *idx += 1;
            };

        if self.scenario == ScenarioName::Custom {
            for spec in &self.custom_streams {
                for _ in 0..spec.count {
                    push(
                        WorkloadKind::Custom,
                        spec.priority,
                        spec.pacing,
                        spec.payload_bytes,
                        spec.chunk_bytes,
                        spec.deadline,
                        &mut out,
                        &mut next_index,
                    );
                }
            }
            return out;
        }

        if self.has_workload(WorkloadKind::Input) {
            push(
                WorkloadKind::Input,
                self.priorities.input,
                Pacing::Hz(self.input.frequency_hz),
                self.input.payload_bytes,
                self.input.payload_bytes,
                Some(self.input.deadline),
                &mut out,
                &mut next_index,
            );
        }
        if self.has_workload(WorkloadKind::Clipboard) {
            let payload = *self.clipboard.payload_sizes.iter().max().unwrap_or(&1024);
            push(
                WorkloadKind::Clipboard,
                self.priorities.clipboard,
                // Clipboard pacing is bursty/random; modeled as a coarse Hz here,
                // refined by the workload generator.
                Pacing::Hz(2),
                payload,
                payload,
                Some(self.clipboard.deadline),
                &mut out,
                &mut next_index,
            );
        }
        if self.has_workload(WorkloadKind::Cast) {
            let per = self.cast.bitrate_mbps / self.cast.streams.max(1) as f64;
            for _ in 0..self.cast.streams.max(1) {
                push(
                    WorkloadKind::Cast,
                    self.priorities.cast,
                    Pacing::RateMbps(per),
                    self.cast.chunk_bytes,
                    self.cast.chunk_bytes,
                    None,
                    &mut out,
                    &mut next_index,
                );
            }
        }
        if self.has_workload(WorkloadKind::File) {
            let streams = self.file.streams.max(1);
            let pacing = match self.file.mode {
                FileMode::Saturating => Pacing::Saturating,
                FileMode::FixedRate => {
                    Pacing::RateMbps(self.file.rate_mbps.unwrap_or(0.0) / streams as f64)
                }
            };
            for _ in 0..streams {
                push(
                    WorkloadKind::File,
                    self.priorities.file,
                    pacing,
                    self.file.chunk_bytes,
                    self.file.chunk_bytes,
                    None,
                    &mut out,
                    &mut next_index,
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_presets_match_documented_order() {
        let g = PriorityConfig::graded();
        assert_eq!(
            (g.ack, g.input, g.clipboard, g.cast, g.file),
            (40, 30, 20, 10, 0)
        );
        assert_eq!(
            PriorityConfig::equal(),
            PriorityConfig {
                ack: 0,
                input: 0,
                clipboard: 0,
                cast: 0,
                file: 0
            }
        );
    }

    #[test]
    fn mixed_resolves_all_four_kinds_with_dense_indices() {
        let cfg = RunConfig::default(); // Mixed
        let streams = cfg.resolve_streams();
        let kinds: Vec<_> = streams.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&WorkloadKind::Input));
        assert!(kinds.contains(&WorkloadKind::File));
        // Dense 0..n indices.
        for (i, s) in streams.iter().enumerate() {
            assert_eq!(s.index as usize, i);
        }
        // Input is a measured probe; File is load.
        let input = streams
            .iter()
            .find(|s| s.kind == WorkloadKind::Input)
            .unwrap();
        assert!(input.measured);
        let file = streams
            .iter()
            .find(|s| s.kind == WorkloadKind::File)
            .unwrap();
        assert!(!file.measured);
    }

    #[test]
    fn input_only_opens_no_background_streams() {
        let cfg = RunConfig {
            scenario: ScenarioName::InputOnly,
            ..Default::default()
        };
        let streams = cfg.resolve_streams();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].kind, WorkloadKind::Input);
    }

    #[test]
    fn custom_stream_grammar_probe_and_load() {
        let probe = StreamSpec::parse("prio=30,hz=125,payload=64,deadline=100ms").unwrap();
        assert_eq!(probe.priority, 30);
        assert_eq!(probe.pacing, Pacing::Hz(125));
        assert!(probe.measured());
        assert_eq!(probe.deadline, Some(Duration::from_millis(100)));

        let load = StreamSpec::parse("prio=0,rate=800mbps,chunk=65536,count=2").unwrap();
        assert_eq!(load.pacing, Pacing::RateMbps(800.0));
        assert_eq!(load.count, 2);
        assert!(!load.measured());

        let sat = StreamSpec::parse("prio=0,saturating").unwrap();
        assert_eq!(sat.pacing, Pacing::Saturating);
        assert!(!sat.measured());
    }

    #[test]
    fn custom_stream_grammar_rejects_bad_specs() {
        assert!(StreamSpec::parse("hz=125").is_err()); // no prio
        assert!(StreamSpec::parse("prio=1").is_err()); // no pacing
        assert!(StreamSpec::parse("prio=1,rate=5mbps,saturating").is_err()); // both
        assert!(StreamSpec::parse("prio=1,rate=-3mbps").is_err()); // negative rate
    }

    #[test]
    fn custom_scenario_resolves_specs_with_counts() {
        let cfg = RunConfig {
            scenario: ScenarioName::Custom,
            custom_streams: vec![
                StreamSpec::parse("prio=30,hz=125,deadline=100ms").unwrap(),
                StreamSpec::parse("prio=0,saturating,count=2").unwrap(),
            ],
            ..Default::default()
        };
        cfg.validate().unwrap();
        let streams = cfg.resolve_streams();
        assert_eq!(streams.len(), 3); // 1 probe + 2 load
        assert_eq!(streams.iter().filter(|s| s.measured).count(), 1);
        assert!(streams.iter().all(|s| s.kind == WorkloadKind::Custom));
    }

    #[test]
    fn validate_rejects_empty_custom_and_bad_warmup() {
        let empty = RunConfig {
            scenario: ScenarioName::Custom,
            ..Default::default()
        };
        assert!(empty.validate().is_err());
        let bad = RunConfig {
            warmup: Duration::from_secs(10),
            cooldown: Duration::from_secs(10),
            duration: Duration::from_secs(15),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }
}
