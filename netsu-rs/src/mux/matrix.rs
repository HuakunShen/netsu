//! The experiment matrix: a fixed set of named cases (scenario × priority ×
//! config variations) run and aggregated so the headline
//! `load_induced_input_p99_delta` — how much competing file load inflates the
//! input probe's p99 — can be read off directly.

use serde::Serialize;

use crate::mux::config::{
    FileMode, PriorityChangeConfig, PriorityConfig, RunConfig, ScenarioName, WorkloadKind,
};
use crate::mux::result::MuxResult;

/// One matrix case: a name plus the config to run.
#[derive(Debug, Clone)]
pub struct MatrixCase {
    pub name: String,
    pub description: String,
    pub config: RunConfig,
}

/// The `required-v1` case set, derived from `base` (which supplies duration,
/// warmup, seed, and the per-kind knobs). Names are stable so aggregation can
/// pair the loaded/unloaded input cases.
pub fn required_v1(base: &RunConfig) -> Vec<MatrixCase> {
    let case = |name: &str, description: &str, f: &dyn Fn(&mut RunConfig)| {
        let mut config = base.clone();
        f(&mut config);
        MatrixCase { name: name.to_string(), description: description.to_string(), config }
    };

    vec![
        case("input-unloaded", "input probe alone (baseline)", &|c| {
            c.scenario = ScenarioName::InputOnly;
        }),
        case("file-saturating", "file load alone", &|c| {
            c.scenario = ScenarioName::FileOnly;
            c.file.mode = FileMode::Saturating;
        }),
        case("input-file-equal", "input + file, equal priority", &|c| {
            c.scenario = ScenarioName::InputFile;
            c.priorities = PriorityConfig::equal();
        }),
        case("input-file-graded", "input + file, input prioritized", &|c| {
            c.scenario = ScenarioName::InputFile;
            c.priorities = PriorityConfig::graded();
        }),
        case("mixed-equal", "all kinds, equal priority", &|c| {
            c.scenario = ScenarioName::Mixed;
            c.priorities = PriorityConfig::equal();
        }),
        case("mixed-graded", "all kinds, graded priority", &|c| {
            c.scenario = ScenarioName::Mixed;
            c.priorities = PriorityConfig::graded();
        }),
        case("mixed-inverted", "all kinds, inverted priority", &|c| {
            c.scenario = ScenarioName::Mixed;
            c.priorities = PriorityConfig::inverted();
        }),
        case("starvation", "high-priority cast vs low-priority file", &|c| {
            c.scenario = ScenarioName::Mixed;
            c.priorities = PriorityConfig { ack: 40, input: 30, clipboard: 20, cast: 40, file: 0 };
            c.cast.streams = 4;
        }),
        case("dynamic-priority-change", "raise file priority mid-run", &|c| {
            c.scenario = ScenarioName::InputFile;
            c.priorities = PriorityConfig::graded();
            c.priority_change = Some(PriorityChangeConfig {
                after: c.duration / 2,
                workload: WorkloadKind::File,
                new_priority: 35,
            });
        }),
    ]
}

/// Per-case aggregate over its repetition results.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatrixAggregate {
    pub name: String,
    pub input_p99_us: Option<u64>,
    pub total_throughput_mbps: f64,
    pub jain_fairness: f64,
}

/// The comparison report written at the end of a matrix run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonReport {
    pub profile: String,
    pub cases: Vec<MatrixAggregate>,
    /// Loaded (`input-file-equal`) input p99 − unloaded (`input-unloaded`) p99,
    /// the headline "how much does load cost the probe" number.
    pub load_induced_input_p99_delta_us: Option<i64>,
}

/// Aggregate results (case name → its result) into a comparison report. When a
/// case ran multiple repetitions, pass each; means are taken per case.
pub fn aggregate(profile: &str, results: &[(String, MuxResult)]) -> ComparisonReport {
    use std::collections::BTreeMap;
    let mut by_case: BTreeMap<String, Vec<&MuxResult>> = BTreeMap::new();
    for (name, r) in results {
        by_case.entry(name.clone()).or_default().push(r);
    }

    let mean = |xs: &[f64]| -> f64 {
        if xs.is_empty() { 0.0 } else { xs.iter().sum::<f64>() / xs.len() as f64 }
    };

    let mut cases = Vec::new();
    for (name, runs) in &by_case {
        let p99s: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.aggregate.probe_p99_us.map(|v| v as f64))
            .collect();
        let input_p99_us = if p99s.is_empty() { None } else { Some(mean(&p99s) as u64) };
        cases.push(MatrixAggregate {
            name: name.clone(),
            input_p99_us,
            total_throughput_mbps: mean(
                &runs.iter().map(|r| r.aggregate.total_throughput_mbps).collect::<Vec<_>>(),
            ),
            jain_fairness: mean(
                &runs.iter().map(|r| r.aggregate.jain_fairness).collect::<Vec<_>>(),
            ),
        });
    }

    let p99_of = |name: &str| -> Option<u64> {
        cases.iter().find(|c| c.name == name).and_then(|c| c.input_p99_us)
    };
    let load_induced_input_p99_delta_us = match (p99_of("input-file-equal"), p99_of("input-unloaded")) {
        (Some(loaded), Some(unloaded)) => Some(loaded as i64 - unloaded as i64),
        _ => None,
    };

    ComparisonReport { profile: profile.to_string(), cases, load_induced_input_p99_delta_us }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_v1_has_stable_named_cases() {
        let cases = required_v1(&RunConfig::default());
        let names: Vec<_> = cases.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"input-unloaded"));
        assert!(names.contains(&"input-file-equal"));
        assert!(names.contains(&"dynamic-priority-change"));
        // Every case validates.
        for c in &cases {
            c.config.validate().unwrap_or_else(|e| panic!("{}: {e}", c.name));
        }
    }

    #[test]
    fn load_induced_delta_uses_named_pair() {
        let mk = |name: &str, p99: u64| {
            let mut r = MuxResult::from_outcome(
                &crate::mux::runner::MuxOutcome {
                    run_id: uuid::Uuid::from_u128(0),
                    scenario: ScenarioName::InputFile,
                    duration: std::time::Duration::from_secs(1),
                    measure_window: std::time::Duration::from_millis(700),
                    streams: vec![],
                    resources: None,
                },
                1,
            );
            r.aggregate.probe_p99_us = Some(p99);
            (name.to_string(), r)
        };
        let report = aggregate(
            "test",
            &[mk("input-unloaded", 1000), mk("input-file-equal", 4000)],
        );
        assert_eq!(report.load_induced_input_p99_delta_us, Some(3000));
    }
}
