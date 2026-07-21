//! The mux runner (client side): opens one bi-stream per resolved stream, sets
//! its QUIC priority, paces its payload, and — for measured streams — matches
//! echoed sequence numbers back to send times for per-message RTT. Warmup and
//! cooldown windows bound which messages count.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, ensure};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::time::Instant;
use uuid::Uuid;

use crate::mux::config::{
    PriorityChangeConfig, ResolvedStream, RunConfig, ScenarioName, WorkloadKind,
};
use crate::mux::metrics::{LatencyRecorder, LatencySummary};
use crate::mux::protocol::{
    Control, DATA_HEADER_LEN, PROTOCOL_VERSION, Start, StreamHello, read_echo, read_frame,
    write_data, write_frame,
};
use crate::mux::workload::{Pacer, StreamProducer};

/// Per-stream result of a mux run.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    pub kind: WorkloadKind,
    pub index: u16,
    pub priority: i32,
    pub measured: bool,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub latency: Option<LatencySummary>,
    /// Per-message `(elapsed_us_since_start, rtt_us)` for measured streams.
    pub rtt_samples: Vec<(u64, u64)>,
}

/// The outcome of one mux run.
#[derive(Debug, Clone)]
pub struct MuxOutcome {
    pub run_id: Uuid,
    pub scenario: ScenarioName,
    pub duration: Duration,
    /// The measured window (duration − warmup − cooldown); throughput/latency
    /// counts come from here.
    pub measure_window: Duration,
    pub streams: Vec<StreamOutcome>,
    pub resources: Option<crate::mux::resources::ResourceSummary>,
}

type SendTimes = Arc<StdMutex<HashMap<u64, Instant>>>;

/// A live per-stream snapshot for the TUI dashboard.
#[derive(Debug, Clone)]
pub struct LiveStream {
    pub index: u16,
    pub kind: WorkloadKind,
    pub priority: i32,
    pub measured: bool,
    pub bytes_sent: u64,
}

/// A periodic snapshot emitted during a run when a live observer is attached.
#[derive(Debug, Clone)]
pub struct LiveSnapshot {
    pub elapsed_ms: u64,
    pub streams: Vec<LiveStream>,
}

/// Optional live-metrics sink; headless runs pass `None`.
pub type LiveObserver = Option<tokio::sync::mpsc::UnboundedSender<LiveSnapshot>>;

struct StreamTasks {
    stream: ResolvedStream,
    bytes_sent: Arc<AtomicU64>,
    send_times: SendTimes,
    writer: tokio::task::JoinHandle<anyhow::Result<()>>,
    reader: Option<tokio::task::JoinHandle<Vec<(u64, u64)>>>,
}

/// Run a mux test over `connection`, returning per-stream throughput + latency.
pub async fn run(connection: &Connection, config: &RunConfig) -> anyhow::Result<MuxOutcome> {
    run_with_live(connection, config, None).await
}

/// As [`run`], but emits a [`LiveSnapshot`] to `live` roughly every 200 ms for
/// a live dashboard.
pub async fn run_with_live(
    connection: &Connection,
    config: &RunConfig,
    live: LiveObserver,
) -> anyhow::Result<MuxOutcome> {
    config.validate()?;
    let streams = config.resolve_streams();
    ensure!(!streams.is_empty(), "scenario resolved to no streams");
    let run_id = Uuid::new_v4();

    let (mut control_send, mut control_recv) = connection
        .open_bi()
        .await
        .context("open mux control stream")?;
    write_frame(
        &mut control_send,
        &Start {
            version: PROTOCOL_VERSION,
            run_id,
            stream_count: streams.len() as u16,
        },
    )
    .await?;
    let ready: Control = read_frame(&mut control_recv).await.context("read Ready")?;
    ensure!(matches!(ready, Control::Ready), "server did not send Ready");

    let resource_sampler = crate::mux::resources::ResourceSampler::start();
    let start = Instant::now();
    let deadline = start + config.duration;
    let warmup_end = start + config.warmup;
    let measure_end = deadline - config.cooldown;

    let mut tasks: Vec<StreamTasks> = Vec::new();
    for stream in &streams {
        let (mut send, recv) = connection.open_bi().await.context("open mux data stream")?;
        let _ = send.set_priority(stream.priority);
        write_frame(
            &mut send,
            &StreamHello {
                version: PROTOCOL_VERSION,
                run_id,
                kind: stream.kind,
                index: stream.index,
                measured: stream.measured,
            },
        )
        .await?;

        let send_times: SendTimes = Arc::new(StdMutex::new(HashMap::new()));
        let bytes_sent = Arc::new(AtomicU64::new(0));

        let writer = {
            let stream = stream.clone();
            let send_times = send_times.clone();
            let bytes_sent = bytes_sent.clone();
            let seed = config.seed;
            let change = config.priority_change.clone();
            tokio::spawn(async move {
                write_stream(
                    send,
                    stream,
                    seed,
                    start,
                    deadline,
                    warmup_end,
                    measure_end,
                    send_times,
                    bytes_sent,
                    change,
                )
                .await
            })
        };

        let reader = if stream.measured {
            let send_times = send_times.clone();
            Some(tokio::spawn(async move {
                read_echoes(recv, send_times, start).await
            }))
        } else {
            None
        };

        tasks.push(StreamTasks {
            stream: stream.clone(),
            bytes_sent,
            send_times,
            writer,
            reader,
        });
    }

    // Live snapshots for a dashboard: read the shared per-stream byte counters
    // every ~200 ms until the deadline.
    if let Some(tx) = live {
        let meta: Vec<(u16, WorkloadKind, i32, bool, Arc<AtomicU64>)> = tasks
            .iter()
            .map(|t| {
                (
                    t.stream.index,
                    t.stream.kind,
                    t.stream.priority,
                    t.stream.measured,
                    t.bytes_sent.clone(),
                )
            })
            .collect();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(200));
            loop {
                tick.tick().await;
                let elapsed = start.elapsed();
                let streams = meta
                    .iter()
                    .map(|(i, k, p, m, b)| LiveStream {
                        index: *i,
                        kind: *k,
                        priority: *p,
                        measured: *m,
                        bytes_sent: b.load(Ordering::Relaxed),
                    })
                    .collect();
                if tx
                    .send(LiveSnapshot {
                        elapsed_ms: elapsed.as_millis() as u64,
                        streams,
                    })
                    .is_err()
                {
                    break;
                }
                if elapsed >= deadline.saturating_duration_since(start) {
                    break;
                }
            }
        });
    }

    // Let the test run, then tear down in order: writers finish sending, readers
    // drain the last echoes, then Stop → Summary on the control stream.
    tokio::time::sleep_until(deadline).await;
    let resources = Some(resource_sampler.stop().await);

    let mut outcomes = Vec::new();
    let mut pending = Vec::new();
    for t in tasks {
        let _ = t.writer.await; // best-effort: a stream error shouldn't sink the run
        let samples = match t.reader {
            Some(r) => r.await.unwrap_or_default(),
            None => Vec::new(),
        };
        pending.push((t.stream, t.bytes_sent, t.send_times, samples));
    }

    write_frame(&mut control_send, &Control::Stop).await?;
    let summary: Control = read_frame(&mut control_recv)
        .await
        .context("read Summary")?;
    let received: HashMap<u16, u64> = match summary {
        Control::Summary { received } => received.into_iter().collect(),
        other => anyhow::bail!("expected Summary, got {other:?}"),
    };

    for (stream, bytes_sent, send_times, samples) in pending {
        let latency = if stream.measured {
            let mut rec = LatencyRecorder::new(stream.deadline);
            for (_, rtt_us) in &samples {
                rec.record(Duration::from_micros(*rtt_us));
            }
            // Anything never echoed within the window is a timeout.
            let leftover = send_times.lock().unwrap().len();
            for _ in 0..leftover {
                rec.record_timeout();
            }
            Some(rec.summary())
        } else {
            None
        };
        outcomes.push(StreamOutcome {
            kind: stream.kind,
            index: stream.index,
            priority: stream.priority,
            measured: stream.measured,
            bytes_sent: bytes_sent.load(Ordering::Relaxed),
            bytes_received: received.get(&stream.index).copied().unwrap_or(0),
            latency,
            rtt_samples: samples,
        });
    }

    let measure_window = config
        .duration
        .saturating_sub(config.warmup)
        .saturating_sub(config.cooldown);
    Ok(MuxOutcome {
        run_id,
        scenario: config.scenario,
        duration: config.duration,
        measure_window,
        streams: outcomes,
        resources,
    })
}

#[allow(clippy::too_many_arguments)]
async fn write_stream(
    mut send: SendStream,
    stream: ResolvedStream,
    seed: u64,
    start: Instant,
    deadline: Instant,
    warmup_end: Instant,
    measure_end: Instant,
    send_times: SendTimes,
    bytes_sent: Arc<AtomicU64>,
    change: Option<PriorityChangeConfig>,
) -> anyhow::Result<()> {
    let mut producer = StreamProducer::new(seed, &stream);
    let mut pacer = producer.interval().map(|i| Pacer::new(i, start));
    let mut seq: u64 = 0;
    let mut change_applied = false;

    loop {
        match pacer.as_mut() {
            Some(p) => {
                let (scheduled, _) = p.wait().await;
                if scheduled >= deadline {
                    break;
                }
            }
            None => {
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }

        if !change_applied
            && let Some(c) = &change
            && c.workload == stream.kind
            && now.saturating_duration_since(start) >= c.after
        {
            let _ = send.set_priority(c.new_priority);
            change_applied = true;
        }

        let in_window = now >= warmup_end && now < measure_end;
        let payload = producer.next_payload();
        if stream.measured && in_window {
            send_times.lock().unwrap().insert(seq, now);
        }
        write_data(&mut send, seq, in_window, &payload).await?;
        bytes_sent.fetch_add((DATA_HEADER_LEN + payload.len()) as u64, Ordering::Relaxed);
        seq += 1;

        if pacer.is_none() {
            // Saturating: yield so this task can't starve the runtime.
            tokio::task::yield_now().await;
        }
    }
    let _ = send.finish();
    Ok(())
}

/// Read echoed sequence numbers, matching each to its send time. Returns
/// `(elapsed_us_since_start, rtt_us)` per matched message.
async fn read_echoes(
    mut recv: RecvStream,
    send_times: SendTimes,
    start: Instant,
) -> Vec<(u64, u64)> {
    let mut samples = Vec::new();
    while let Ok(Some(seq)) = read_echo(&mut recv).await {
        let sent = send_times.lock().unwrap().remove(&seq);
        if let Some(sent) = sent {
            let now = Instant::now();
            let rtt = now.saturating_duration_since(sent);
            let elapsed = now.saturating_duration_since(start);
            samples.push((elapsed.as_micros() as u64, rtt.as_micros() as u64));
        }
    }
    samples
}
