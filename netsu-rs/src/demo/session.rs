//! Controller/controlled session over one iroh connection: a control bi-stream
//! carrying input events, plus N uni bulk-load streams so latency can be felt
//! under load. Streamlined from the source demo; iroh-only.

use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::time::Instant as TokioInstant;
use uuid::Uuid;

use crate::demo::DEMO_ALPN;
use crate::demo::input::{InputGate, InputQueue, NormalizedInputEvent, PressedState};
use crate::demo::monio_backend::{
    MonioCapture, MonioInjector, is_alt_key, is_control_key, is_emergency_key, normalize_event,
};
use crate::demo::input::InputInjector;
use crate::mux::config::WorkloadKind;
use crate::mux::protocol::{read_frame, write_frame};
use crate::mux::workload::{DeterministicBytes, Pacer};
use crate::p2p::endpoint;

const DEMO_VERSION: u16 = 1;
const BULK_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub session_id: Uuid,
    pub sequence: u64,
    pub controller_queue_age_us: u64,
    pub event: NormalizedInputEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DemoFrame {
    Hello { version: u16, session_id: Uuid, bulk_streams: u16 },
    Ready { version: u16, session_id: Uuid },
    Input(EventEnvelope),
    Finish { version: u16, session_id: Uuid },
    Finished { version: u16, session_id: Uuid, bulk_bytes_received: u64 },
}

pub struct ControllerConfig {
    pub peer: String,
    pub duration: Duration,
    pub bulk_streams: u16,
    pub bulk_rate_mbps: Option<f64>,
    pub hook_capacity: usize,
    pub direct_only: bool,
    pub no_rendezkey: bool,
    pub rendezkey_url: Option<String>,
}

pub struct ControlledConfig {
    pub allow_peer: Option<String>,
    pub inject_input: bool,
    pub idle_timeout: Duration,
    pub direct_only: bool,
    pub no_rendezkey: bool,
    pub rendezkey_url: Option<String>,
}

/// Controller: capture local input and stream it to the peer, plus bulk load.
pub async fn run_controller(config: ControllerConfig) -> anyhow::Result<()> {
    let url = config
        .rendezkey_url
        .clone()
        .unwrap_or_else(|| crate::p2p::rendezkey::DEFAULT_BASE_URL.to_string());
    let ticket = if config.no_rendezkey {
        config.peer.clone()
    } else {
        crate::p2p::addr::resolve_ticket(&config.peer, &url).await?
    };
    let peer = endpoint::parse_ticket(&ticket)?;
    let ep = endpoint::bind_client(config.direct_only, true).await?;
    let connection = endpoint::connect(&ep, peer, DEMO_ALPN).await?;

    let session_id = Uuid::new_v4();
    let (mut ctrl_send, mut ctrl_recv) = connection.open_bi().await.context("open control")?;
    write_frame(
        &mut ctrl_send,
        &DemoFrame::Hello { version: DEMO_VERSION, session_id, bulk_streams: config.bulk_streams },
    )
    .await?;
    let ready: DemoFrame = read_frame(&mut ctrl_recv).await?;
    ensure!(matches!(ready, DemoFrame::Ready { .. }), "peer did not send Ready");

    // Bulk-load pumps.
    let deadline = TokioInstant::now() + config.duration;
    for i in 0..config.bulk_streams {
        let mut send = connection.open_uni().await.context("open bulk stream")?;
        let rate = config.bulk_rate_mbps.map(|r| r / config.bulk_streams.max(1) as f64);
        tokio::spawn(async move {
            let mut bytes = DeterministicBytes::new(0xB0_1C, WorkloadKind::Custom, i);
            let interval = rate
                .filter(|r| *r > 0.0)
                .map(|r| Duration::from_secs_f64(BULK_CHUNK as f64 * 8.0 / (r * 1e6)));
            let mut pacer = interval.map(|iv| Pacer::new(iv, TokioInstant::now()));
            loop {
                if let Some(p) = pacer.as_mut() {
                    p.wait().await;
                }
                if TokioInstant::now() >= deadline {
                    break;
                }
                let payload = bytes.chunk(BULK_CHUNK);
                if tokio::io::AsyncWriteExt::write_all(&mut send, &payload).await.is_err() {
                    break;
                }
                if pacer.is_none() {
                    tokio::task::yield_now().await;
                }
            }
            let _ = send.finish();
        });
    }

    // Capture loop.
    let capture = MonioCapture::start(config.hook_capacity)?;
    let MonioCapture { handle: _handle, mut events, display } = capture;
    let mut queue = InputQueue::new(config.hook_capacity)?;
    let mut pressed = PressedState::new();
    let mut sequence = 0u64;
    let (mut ctrl_held, mut alt_held) = (false, false);

    println!("controller: streaming input — press 'q' or Escape+Ctrl+Alt to stop");
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            maybe = events.recv() => {
                let Some(raw) = maybe else { break };
                let Some(norm) = normalize_event(&raw, &display)? else { continue };
                if let Some(down) = is_control_key(&norm) { ctrl_held = down; }
                if let Some(down) = is_alt_key(&norm) { alt_held = down; }
                if norm.is_local_quit() || (is_emergency_key(&norm) && ctrl_held && alt_held) {
                    break;
                }
                pressed.observe(&norm);
                queue.push(norm)?;
                while let Some(captured) = queue.pop_captured() {
                    sequence += 1;
                    let env = EventEnvelope {
                        session_id,
                        sequence,
                        controller_queue_age_us: captured.captured_at.elapsed().as_micros() as u64,
                        event: captured.event,
                    };
                    write_frame(&mut ctrl_send, &DemoFrame::Input(env)).await?;
                }
            }
        }
    }

    // Release everything, then finish.
    sequence += 1;
    let release = EventEnvelope {
        session_id,
        sequence,
        controller_queue_age_us: 0,
        event: NormalizedInputEvent::ReleaseAll,
    };
    write_frame(&mut ctrl_send, &DemoFrame::Input(release)).await?;
    write_frame(&mut ctrl_send, &DemoFrame::Finish { version: DEMO_VERSION, session_id }).await?;
    if let Ok(DemoFrame::Finished { bulk_bytes_received, .. }) =
        read_frame::<_, DemoFrame>(&mut ctrl_recv).await
    {
        println!("controller: peer received {bulk_bytes_received} bulk bytes");
    }
    connection.close(0u32.into(), b"done");
    ep.close().await;
    Ok(())
}

/// Controlled: accept a controller, inject its input (if opted in), drain bulk.
pub async fn run_controlled(config: ControlledConfig) -> anyhow::Result<()> {
    let (endpoint, ticket) =
        endpoint::bind_listener_with_ticket(DEMO_ALPN, config.direct_only, true).await?;
    println!("controlled: waiting for a controller");
    if !config.no_rendezkey {
        let url = config.rendezkey_url.as_deref().unwrap_or(crate::p2p::rendezkey::DEFAULT_BASE_URL);
        if let Some(token) = crate::p2p::rendezkey::token_from_env() {
            match crate::p2p::rendezkey::store(url, &token, &ticket, 3600, 5).await {
                Ok(code) => println!("code:   {code}"),
                Err(e) => eprintln!("(rendez-key unavailable: {e:#})"),
            }
        }
    }
    println!("ticket: {ticket}");
    if config.inject_input {
        println!("controlled: INPUT INJECTION ENABLED");
    } else {
        println!("controlled: injection OFF (pass --inject-input to enable)");
    }

    let incoming =
        endpoint.accept().await.context("listener closed before a controller arrived")?;
    let connection = incoming.await.context("accept controller")?;

    if let Some(allowed) = &config.allow_peer {
        let remote = connection.remote_id().to_string();
        ensure!(
            remote == *allowed,
            "rejecting peer {remote}: not the --allow-peer endpoint"
        );
    }

    let (mut ctrl_send, mut ctrl_recv) =
        connection.accept_bi().await.context("accept control")?;
    let hello: DemoFrame = read_frame(&mut ctrl_recv).await?;
    let (session_id, bulk_streams) = match hello {
        DemoFrame::Hello { session_id, bulk_streams, .. } => (session_id, bulk_streams),
        other => bail!("expected Hello, got {other:?}"),
    };
    write_frame(&mut ctrl_send, &DemoFrame::Ready { version: DEMO_VERSION, session_id }).await?;

    // Drain bulk streams.
    let bulk_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    for _ in 0..bulk_streams {
        let mut recv = connection.accept_uni().await.context("accept bulk stream")?;
        let bulk_bytes = bulk_bytes.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; BULK_CHUNK];
            while let Ok(n) = recv.read(&mut buf).await {
                match n {
                    Some(0) | None => break,
                    Some(read) => {
                        bulk_bytes.fetch_add(read as u64, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
    }

    let injector = if config.inject_input { Some(MonioInjector::new()?) } else { None };
    let mut gate = InputGate::new(config.idle_timeout);
    let mut pressed = PressedState::new();

    loop {
        let frame: DemoFrame = match read_frame(&mut ctrl_recv).await {
            Ok(f) => f,
            Err(_) => break, // controller disconnected
        };
        match frame {
            DemoFrame::Input(env) => {
                let age = Duration::from_micros(env.controller_queue_age_us);
                if !gate.accept(env.sequence, age) {
                    continue;
                }
                pressed.observe(&env.event);
                if let Some(injector) = &injector {
                    match &env.event {
                        NormalizedInputEvent::ReleaseAll => {
                            for e in pressed.release_events() {
                                let _ = injector.inject(&e);
                            }
                        }
                        e => {
                            let _ = injector.inject(e);
                        }
                    }
                }
            }
            DemoFrame::Finish { .. } => break,
            _ => {}
        }
    }

    // Safety: release everything held before exiting.
    if let Some(injector) = &injector {
        for e in pressed.release_events() {
            let _ = injector.inject(&e);
        }
    }
    let received = bulk_bytes.load(std::sync::atomic::Ordering::Relaxed);
    let _ = write_frame(
        &mut ctrl_send,
        &DemoFrame::Finished { version: DEMO_VERSION, session_id, bulk_bytes_received: received },
    )
    .await;
    println!("controlled: session ended, drained {received} bulk bytes");
    endpoint.close().await;
    Ok(())
}
