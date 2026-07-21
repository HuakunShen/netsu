//! Data-plane send/receive loops for a single data stream. Pure payload
//! movement: no framing, no protocol state, no knowledge of TCP vs. WS vs.
//! UDP — everything here is expressed against [`DataChannel`]. Stream
//! lifecycle (spawning, shutdown signaling, teardown) is owned by
//! `client.rs`'s control state machine.

use std::sync::Arc;

use rand::Rng;
use tokio::sync::{Mutex, watch};

use crate::stats::IntervalMeter;
use crate::streams::channel::DataChannel;

/// Mutable per-stream accounting shared by client and (later) server.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamCounters {
    pub id: u32,
    pub bytes: u64,
    pub packets: u64,
    pub jitter: f64, // seconds
    pub errors: u64,
}

impl StreamCounters {
    pub fn new(id: u32) -> Self {
        StreamCounters {
            id,
            ..Default::default()
        }
    }
}

/// Stream `id` values as iperf3 actually assigns them (`iperf_add_stream` in
/// `iperf_api.c`), NOT a plain `1..N` sequence. The first stream gets id 1;
/// every subsequent stream's id is `existing_count + 2` — a historical quirk
/// iperf3's own source comments acknowledge and preserve for compatibility.
/// For `N` streams this produces `1, 3, 4, 5, ..., N+1`.
///
/// This matters because EXCHANGE_RESULTS's per-stream `id` is never
/// negotiated on the wire — each side matches an incoming result's `id`
/// against ids it independently assigned during CREATE_STREAMS, so both
/// peers must derive ids identically using this same formula, in the same
/// stream-creation order, or real iperf3 rejects the exchange with "stream
/// has an invalid id" (see `PROTOCOL.md`'s "EXCHANGE_RESULTS JSON" note).
/// `existing_count` is how many streams this peer has already added before
/// the one being assigned now.
pub fn next_stream_id(existing_count: usize) -> u32 {
    if existing_count == 0 {
        1
    } else {
        existing_count as u32 + 2
    }
}

/// Shared handle to a data channel: sender/receiver tasks lock it briefly per
/// I/O call, and the control loop locks it to `close()` at teardown or to
/// read a latched transfer error via [`DataChannel::error`] — all without
/// needing a separate ad hoc error-latching mechanism, since `DataChannel`
/// already provides one.
pub type SharedChannel = Arc<Mutex<Box<dyn DataChannel>>>;
pub type SharedCounters = Arc<Mutex<StreamCounters>>;
pub type SharedMeter = Arc<Mutex<IntervalMeter>>;

/// Send a fixed random chunk (defeats link compression, matching real
/// iperf3's payload) repeatedly until `shutdown` fires.
///
/// In tokio, `channel.write_chunk(..).await` yields to the runtime whenever
/// the write can't complete synchronously, and tokio's cooperative
/// scheduling budget forces a yield even when it can — unlike a Node/JS
/// event loop, there is no risk of a hot loop of synchronously-resolved
/// writes starving the runtime's timers, so no explicit yield is needed here
/// (see `runner.ts`'s `startSender` for the Node-specific workaround this
/// port does not need).
///
/// Cancel safety: `shutdown.changed()` races directly against the in-flight
/// write. If `shutdown` fires while a write is pending, the write future is
/// dropped (a partial write is acceptable — the stream is being torn down
/// regardless) rather than blocking the caller who flipped `shutdown` on a
/// socket that may never drain (e.g. the peer stopped reading after its own
/// TEST_END handling). `changed()` (not `wait_for`, which resolves to a lock
/// guard) is used deliberately: a guard held across the sibling branch's
/// `.await` would make the whole `select!` future `!Send`, and this task is
/// spawned via `tokio::spawn`, which requires `Send`.
pub async fn run_sender(
    channel: SharedChannel,
    counters: SharedCounters,
    meter: SharedMeter,
    len: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut chunk = vec![0u8; len.max(1)];
    rand::thread_rng().fill(&mut chunk[..]);

    loop {
        // Synchronous, not held across an `.await`: safe even though the
        // borrow guard itself isn't `Send`.
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            result = async {
                let mut ch = channel.lock().await;
                ch.write_chunk(&chunk).await
            } => {
                if result.is_err() {
                    // The channel latches the error internally (see
                    // `DataChannel::error`); the control loop checks it at
                    // EXCHANGE_RESULTS time. Nothing further to do here but
                    // stop sending.
                    break;
                }
                let n = chunk.len() as u64;
                counters.lock().await.bytes += n;
                meter.lock().await.add(n);
            }
        }
    }
}

/// Read whatever arrives until the channel closes (`Ok(0)`/`Err`) or
/// `shutdown` fires.
///
/// Reverse-mode receive streams are intentionally left running through the
/// duration timer and EXCHANGE_RESULTS (see `client.rs`'s protocol-fact-3
/// handling) — `shutdown` here is a *separate* signal from forward-mode's
/// `stop_senders`, only fired at final teardown, once the peer has had its
/// chance to finish writing on its own. It exists because `read_chunk` has no
/// timeout of its own: on an idle-but-open socket (a half-open connection, a
/// peer that stopped writing without closing) it would otherwise never
/// resolve, and since it's called with the channel's mutex held across the
/// `.await`, that would starve out any other task waiting to lock the same
/// channel (e.g. `client.rs`'s teardown, trying to `close()` it) forever.
///
/// Cancel safety mirrors `run_sender`'s doc exactly: `shutdown.changed()`
/// races directly against the in-flight, lock-holding read. If `shutdown`
/// fires first, the read future (and the `MutexGuard` it holds) is dropped —
/// a partial read is fine, the stream is being torn down regardless — which
/// is what lets a subsequent `close()` actually acquire the lock instead of
/// queuing behind a read that may never finish.
pub async fn run_receiver(
    channel: SharedChannel,
    counters: SharedCounters,
    meter: SharedMeter,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut buf = vec![0u8; 65536];
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            result = async {
                let mut ch = channel.lock().await;
                ch.read_chunk(&mut buf).await
            } => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let n = n as u64;
                        counters.lock().await.bytes += n;
                        meter.lock().await.add(n);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_ids_follow_the_iperf_add_stream_quirk() {
        // -P 3: ids 1, 3, 4 — not 1, 2, 3.
        let mut ids = Vec::new();
        for existing in 0..3 {
            ids.push(next_stream_id(existing));
        }
        assert_eq!(ids, vec![1, 3, 4]);
    }

    #[test]
    fn single_stream_gets_id_one() {
        assert_eq!(next_stream_id(0), 1);
    }
}
