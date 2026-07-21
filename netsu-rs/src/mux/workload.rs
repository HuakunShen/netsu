//! Deterministic, paced payload generation for one stream. Payloads are
//! reproducible (seeded ChaCha8 per stream), and pacing turns a stream's
//! [`Pacing`] into either a fixed inter-message interval or a saturating
//! (as-fast-as-possible) feed.

use std::time::Duration;

use bytes::Bytes;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tokio::time::Instant;

use crate::mux::config::{Pacing, ResolvedStream, WorkloadKind};

/// Reproducible byte source, unique per (seed, kind, index).
pub struct DeterministicBytes {
    rng: ChaCha8Rng,
}

impl DeterministicBytes {
    pub fn new(seed: u64, kind: WorkloadKind, index: u16) -> Self {
        Self {
            rng: ChaCha8Rng::seed_from_u64(derive_seed(seed, kind, index)),
        }
    }

    pub fn chunk(&mut self, len: usize) -> Bytes {
        let mut bytes = vec![0u8; len];
        self.rng.fill_bytes(&mut bytes);
        Bytes::from(bytes)
    }
}

fn derive_seed(seed: u64, kind: WorkloadKind, index: u16) -> u64 {
    let k: u64 = match kind {
        WorkloadKind::Control => 0x10,
        WorkloadKind::Ack => 0x20,
        WorkloadKind::Input => 0x30,
        WorkloadKind::Clipboard => 0x40,
        WorkloadKind::Cast => 0x50,
        WorkloadKind::File => 0x60,
        WorkloadKind::Custom => 0x70,
    };
    seed.rotate_left(17)
        ^ k.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ u64::from(index).wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

/// The inter-message interval for a stream, or `None` for saturating.
pub fn interval_for(pacing: Pacing, chunk_bytes: usize) -> Option<Duration> {
    match pacing {
        Pacing::Saturating => None,
        Pacing::Hz(hz) if hz > 0 => Some(Duration::from_secs_f64(1.0 / hz as f64)),
        Pacing::Hz(_) => None,
        Pacing::RateMbps(mbps) if mbps > 0.0 => {
            let secs = chunk_bytes as f64 * 8.0 / (mbps * 1_000_000.0);
            Some(Duration::from_secs_f64(secs))
        }
        Pacing::RateMbps(_) => None,
    }
}

/// A fixed-interval pacer that does not drift (schedules off a fixed origin).
pub struct Pacer {
    interval: Duration,
    next: Instant,
}

impl Pacer {
    pub fn new(interval: Duration, start: Instant) -> Self {
        Self {
            interval,
            next: start,
        }
    }

    /// Sleep until the next tick; returns `(scheduled, lateness)`.
    pub async fn wait(&mut self) -> (Instant, Duration) {
        let scheduled = self.next;
        tokio::time::sleep_until(scheduled).await;
        let now = Instant::now();
        let lateness = now.saturating_duration_since(scheduled);
        self.next += self.interval;
        (scheduled, lateness)
    }
}

/// Per-stream payload generator: hands out reproducible chunks of the stream's
/// configured size and reports its pacing interval.
pub struct StreamProducer {
    bytes: DeterministicBytes,
    payload_bytes: usize,
    interval: Option<Duration>,
}

impl StreamProducer {
    pub fn new(seed: u64, stream: &ResolvedStream) -> Self {
        Self {
            bytes: DeterministicBytes::new(seed, stream.kind, stream.index),
            payload_bytes: stream.payload_bytes,
            interval: interval_for(stream.pacing, stream.chunk_bytes),
        }
    }

    pub fn interval(&self) -> Option<Duration> {
        self.interval
    }

    /// The next payload chunk.
    pub fn next_payload(&mut self) -> Bytes {
        self.bytes.chunk(self.payload_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::config::Pacing;

    fn stream(pacing: Pacing, payload: usize, chunk: usize) -> ResolvedStream {
        ResolvedStream {
            kind: WorkloadKind::Custom,
            index: 0,
            priority: 0,
            pacing,
            payload_bytes: payload,
            chunk_bytes: chunk,
            deadline: None,
            measured: false,
        }
    }

    #[test]
    fn payloads_are_reproducible_and_stream_scoped() {
        let s = stream(Pacing::Saturating, 32, 32);
        let mut a = StreamProducer::new(7, &s);
        let mut b = StreamProducer::new(7, &s);
        assert_eq!(a.next_payload(), b.next_payload());
        // Different index → different bytes.
        let mut s2 = s.clone();
        s2.index = 1;
        let mut c = StreamProducer::new(7, &s2);
        assert_ne!(a.next_payload(), c.next_payload());
    }

    #[test]
    fn rate_interval_matches_chunk_budget() {
        // 1 Mbps, 125000-byte chunk → 1s per chunk.
        let i = interval_for(Pacing::RateMbps(1.0), 125_000).unwrap();
        assert!((i.as_secs_f64() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hz_interval_and_saturating() {
        assert_eq!(
            interval_for(Pacing::Hz(125), 64),
            Some(Duration::from_secs_f64(1.0 / 125.0))
        );
        assert_eq!(interval_for(Pacing::Saturating, 1000), None);
    }

    #[tokio::test(start_paused = true)]
    async fn pacer_does_not_drift() {
        let start = Instant::now();
        let mut pacer = Pacer::new(Duration::from_millis(10), start);
        let (s0, _) = pacer.wait().await;
        let (s1, _) = pacer.wait().await;
        let (s2, _) = pacer.wait().await;
        assert_eq!(s1.duration_since(s0), Duration::from_millis(10));
        assert_eq!(s2.duration_since(s0), Duration::from_millis(20));
    }
}
