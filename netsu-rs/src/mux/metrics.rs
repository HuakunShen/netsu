//! Latency and fairness accounting for a mux run.

use std::time::Duration;

use hdrhistogram::Histogram;

/// Records per-message RTTs into an HDR histogram (1 µs … 60 s, 3 sig figs) and
/// counts deadline misses.
pub struct LatencyRecorder {
    hist: Histogram<u64>,
    deadline: Option<Duration>,
    deadline_exceeded: u64,
    timeouts: u64,
}

/// A summarized latency distribution, all in microseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct LatencySummary {
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

impl LatencyRecorder {
    pub fn new(deadline: Option<Duration>) -> Self {
        Self {
            hist: Histogram::new_with_bounds(1, 60_000_000, 3).expect("valid histogram bounds"),
            deadline,
            deadline_exceeded: 0,
            timeouts: 0,
        }
    }

    /// Record a completed round-trip.
    pub fn record(&mut self, rtt: Duration) {
        let us = rtt.as_micros().min(u64::MAX as u128) as u64;
        let _ = self.hist.record(us.max(1));
        if let Some(d) = self.deadline
            && rtt > d
        {
            self.deadline_exceeded += 1;
        }
    }

    /// Record a message that never came back within the ACK timeout.
    pub fn record_timeout(&mut self) {
        self.timeouts += 1;
        // A timeout is also a deadline miss.
        self.deadline_exceeded += 1;
    }

    pub fn summary(&self) -> LatencySummary {
        let count = self.hist.len();
        let total = count + self.timeouts;
        LatencySummary {
            count,
            timeout_count: self.timeouts,
            min_us: if count > 0 { self.hist.min() } else { 0 },
            mean_us: if count > 0 { self.hist.mean() } else { 0.0 },
            p50_us: self.hist.value_at_quantile(0.50),
            p90_us: self.hist.value_at_quantile(0.90),
            p99_us: self.hist.value_at_quantile(0.99),
            p999_us: self.hist.value_at_quantile(0.999),
            max_us: if count > 0 { self.hist.max() } else { 0 },
            deadline_exceeded: self.deadline_exceeded,
            deadline_exceeded_rate: if total > 0 {
                self.deadline_exceeded as f64 / total as f64
            } else {
                0.0
            },
        }
    }
}

/// Jain's fairness index over per-stream throughputs: `(Σx)² / (n·Σx²)`.
/// 1.0 = perfectly fair; 1/n = maximally unfair.
pub fn jains_fairness(values: &[f64]) -> f64 {
    let n = values.len();
    if n == 0 {
        return 1.0;
    }
    let sum: f64 = values.iter().sum();
    let sum_sq: f64 = values.iter().map(|x| x * x).sum();
    if sum_sq == 0.0 {
        return 1.0;
    }
    (sum * sum) / (n as f64 * sum_sq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reports_tail_and_deadline_misses() {
        let mut r = LatencyRecorder::new(Some(Duration::from_millis(10)));
        for ms in [1u64, 2, 3, 100] {
            r.record(Duration::from_millis(ms));
        }
        let s = r.summary();
        assert_eq!(s.count, 4);
        assert_eq!(s.deadline_exceeded, 1); // only the 100ms sample
        assert!(s.p99_us >= s.p50_us);
        assert!(s.max_us >= 99_000);
    }

    #[test]
    fn timeouts_count_as_deadline_misses() {
        let mut r = LatencyRecorder::new(Some(Duration::from_millis(10)));
        r.record(Duration::from_millis(1));
        r.record_timeout();
        let s = r.summary();
        assert_eq!(s.count, 1);
        assert_eq!(s.timeout_count, 1);
        assert_eq!(s.deadline_exceeded, 1);
        assert!((s.deadline_exceeded_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fairness_edges() {
        assert!((jains_fairness(&[10.0, 10.0, 10.0]) - 1.0).abs() < 1e-9);
        assert!((jains_fairness(&[]) - 1.0).abs() < 1e-9);
        // One hog, three starved → ~0.25 lower bound behavior.
        let f = jains_fairness(&[100.0, 0.0, 0.0, 0.0]);
        assert!(f > 0.24 && f < 0.26, "got {f}");
    }
}
