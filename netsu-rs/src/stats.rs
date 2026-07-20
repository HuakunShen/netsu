//! Pure math for per-interval throughput reporting and UDP jitter/loss accounting.

use std::time::Instant;

/// Compute bits per second from bytes and duration.
///
/// Returns 0.0 when seconds <= 0.0.
pub fn bits_per_second(bytes: u64, seconds: f64) -> f64 {
    if seconds > 0.0 {
        (bytes as f64 * 8.0) / seconds
    } else {
        0.0
    }
}

/// Report for one interval (start to end).
#[derive(Debug, Clone)]
pub struct IntervalReport {
    /// Start time in seconds since test start
    pub start: f64,
    /// End time in seconds since test start
    pub end: f64,
    /// Bytes received in this interval
    pub bytes: u64,
    /// Bits per second in this interval
    pub bits_per_second: f64,
}

/// Accumulates bytes per interval; snap() closes the current interval and starts the next.
#[derive(Debug)]
pub struct IntervalMeter {
    start: Instant,
    last_snap: Instant,
    total_bytes: u64,
    interval_bytes: u64,
}

impl IntervalMeter {
    /// Create a new meter starting at the given instant.
    pub fn new(start: Instant) -> Self {
        Self {
            start,
            last_snap: start,
            total_bytes: 0,
            interval_bytes: 0,
        }
    }

    /// Add bytes to the current interval and to the total.
    pub fn add(&mut self, bytes: u64) {
        self.total_bytes += bytes;
        self.interval_bytes += bytes;
    }

    /// Close the current interval and return its report; start the next interval.
    pub fn snap(&mut self, now: Instant) -> IntervalReport {
        let duration = now.duration_since(self.last_snap);
        let seconds = duration.as_secs_f64();

        let report = IntervalReport {
            start: self.last_snap.duration_since(self.start).as_secs_f64(),
            end: now.duration_since(self.start).as_secs_f64(),
            bytes: self.interval_bytes,
            bits_per_second: bits_per_second(self.interval_bytes, seconds),
        };

        self.last_snap = now;
        self.interval_bytes = 0;
        report
    }

    /// Get total bytes received across all intervals.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

/// RFC 1889 jitter tracker + loss/reorder accounting for UDP receive side.
#[derive(Debug)]
pub struct JitterTracker {
    jitter_secs: f64,
    prev_transit_micros: Option<i64>,
    max_seq: u32,
    received: u64,
    out_of_order: u64,
}

impl JitterTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self {
            jitter_secs: 0.0,
            prev_transit_micros: None,
            max_seq: 0,
            received: 0,
            out_of_order: 0,
        }
    }

    /// Record a received packet with its sequence number and timestamps.
    ///
    /// # Arguments
    /// * `pcount` - packet sequence number (starts at 1)
    /// * `sent_micros` - packet send time in microseconds
    /// * `now_micros` - packet arrival time in microseconds
    pub fn on_packet(&mut self, pcount: u32, sent_micros: u64, now_micros: u64) {
        self.received += 1;

        if pcount > self.max_seq {
            self.max_seq = pcount;
        } else {
            self.out_of_order += 1;
        }

        let transit_micros = (now_micros as i64) - (sent_micros as i64);

        if let Some(prev_transit) = self.prev_transit_micros {
            let d_micros = (transit_micros.abs_diff(prev_transit)) as f64;
            let d_secs = d_micros / 1_000_000.0;
            self.jitter_secs += (d_secs - self.jitter_secs) / 16.0;
        }

        self.prev_transit_micros = Some(transit_micros);
    }

    /// Get jitter in seconds.
    pub fn jitter_secs(&self) -> f64 {
        self.jitter_secs
    }

    /// Get number of packets received.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Get highest sequence number seen.
    pub fn max_seq(&self) -> u32 {
        self.max_seq
    }

    /// Get number of out-of-order packets (pcount <= max_seq).
    pub fn out_of_order(&self) -> u64 {
        self.out_of_order
    }

    /// Get number of lost packets (max_seq - received, clamped >= 0).
    pub fn lost(&self) -> u64 {
        self.max_seq.saturating_sub(self.received as u32) as u64
    }
}

impl Default for JitterTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn converts_bytes_over_seconds_to_bits_per_second() {
        assert_eq!(bits_per_second(1_000_000, 8.0), 1_000_000.0);
        assert_eq!(bits_per_second(100, 0.0), 0.0);
    }

    #[test]
    fn interval_meter_reports_deltas_and_running_total() {
        let t0 = Instant::now();
        let mut m = IntervalMeter::new(t0);
        m.add(500);
        m.add(500);
        let first = m.snap(t0 + Duration::from_secs(1));
        assert_eq!(first.bytes, 1000);
        assert!((first.start - 0.0).abs() < 1e-9);
        assert!((first.end - 1.0).abs() < 1e-9);
        assert!((first.bits_per_second - 8000.0).abs() < 1e-6);
        m.add(250);
        let second = m.snap(t0 + Duration::from_secs(2));
        assert!((second.start - 1.0).abs() < 1e-9);
        assert_eq!(second.bytes, 250);
        assert_eq!(m.total_bytes(), 1250);
    }

    #[test]
    fn tracks_loss_and_out_of_order_from_packet_counts() {
        let mut t = JitterTracker::new();
        t.on_packet(1, 0, 10_000);
        t.on_packet(2, 10_000, 20_000);
        t.on_packet(5, 40_000, 50_000); // 3,4 missing
        t.on_packet(4, 30_000, 55_000); // 4 arrives late
        assert_eq!(t.received(), 4);
        assert_eq!(t.max_seq(), 5);
        assert_eq!(t.out_of_order(), 1);
        assert_eq!(t.lost(), 1); // 5 expected, 4 received
    }

    #[test]
    fn computes_rfc1889_jitter_hand_computed_sequence() {
        // transit times (ms): 10, 12, 9  ->  d = 2, then 3
        // jitter = 0; then 0 + (2-0)/16 = 0.125; then 0.125 + (3-0.125)/16 = 0.3046875
        let mut t = JitterTracker::new();
        t.on_packet(1, 0, 10_000);
        t.on_packet(2, 100_000, 112_000);
        t.on_packet(3, 200_000, 209_000);
        let jitter_ms = t.jitter_secs() * 1000.0;
        assert!((jitter_ms - 0.3046875).abs() < 1e-4, "got {jitter_ms}");
    }

    #[test]
    fn handles_sender_clock_ahead_of_receiver_no_panic() {
        // Regression test: sender's clock ahead of receiver (sent_micros > now_micros).
        // This caused unsigned u64 underflow (panic in debug, silent wraparound in release).
        // With signed i64, transit becomes negative, which is mathematically valid.
        let mut t = JitterTracker::new();
        // First packet: normal case (now > sent)
        t.on_packet(1, 0, 10_000);
        // Second packet: sender's clock ahead by 100ms, receiver's clock only 50ms ahead
        // sent_micros=200_000, now_micros=150_000  =>  transit=-50_000
        // prev_transit=10_000, d=|−50_000 − 10_000|=60_000
        t.on_packet(2, 200_000, 150_000);

        // Verify no panic occurred and jitter is finite and reasonable
        let jitter_secs = t.jitter_secs();
        assert!(
            jitter_secs.is_finite(),
            "jitter should be finite, got {jitter_secs}"
        );
        assert!(
            jitter_secs >= 0.0,
            "jitter should be non-negative, got {jitter_secs}"
        );
        // Expected: d=60_000 micros = 0.060 secs; jitter = 0 + (0.060 - 0) / 16 = 0.00375
        assert!(
            (jitter_secs - 0.00375).abs() < 1e-6,
            "expected ~0.00375, got {jitter_secs}"
        );
    }
}
