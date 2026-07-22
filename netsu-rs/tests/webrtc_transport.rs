#![cfg(feature = "webrtc")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use netsu::client::{
    ClientOptions, ConnectionInfo, Transport, WebRtcConnectionInfo, connection_json, run_client,
};
use netsu::server::{ServerOptions, start_server};
use netsu::transport::webrtc::WebRtcOptions;
use netsu::transport::webrtc::peer::negotiate_offerer;
use netsu::transport::webrtc::signaling::SignalingClient;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const WORKER_PORT: u16 = 18_788;

struct WorkerdGuard {
    child: Child,
}

impl WorkerdGuard {
    async fn start(repo_root: &Path) -> Self {
        let mut child = Command::new(repo_root.join("scripts/dev-webrtc-signal.sh"))
            .current_dir(repo_root)
            .env("SIGNAL_WORKER_PORT", WORKER_PORT.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start Wrangler signaling wrapper");
        let stdout = child.stdout.take().expect("wrapper stdout");
        // Cold Wrangler module loading can dominate startup on an external
        // workspace; the wrapper only prints after its health check passes.
        let ready = tokio::time::timeout(
            Duration::from_secs(60),
            BufReader::new(stdout).lines().next_line(),
        )
        .await
        .expect("Wrangler readiness timed out")
        .expect("read wrapper readiness")
        .expect("wrapper exited before readiness");
        assert_eq!(
            ready,
            format!("READY http://127.0.0.1:{WORKER_PORT}/v1/signal")
        );
        Self { child }
    }

    async fn shutdown(mut self) {
        if let Some(pid) = self.child.id() {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("Wrangler wrapper did not stop")
            .expect("wait for Wrangler wrapper");
    }
}

impl Drop for WorkerdGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("netsu-rs lives under the repository root")
        .to_path_buf()
}

fn signal_options() -> WebRtcOptions {
    WebRtcOptions::new(
        &format!("http://127.0.0.1:{WORKER_PORT}/v1/signal"),
        std::iter::empty::<&str>(),
        false,
    )
    .unwrap()
}

#[test]
fn webrtc_connection_json_is_stable_and_redacted() {
    let json = connection_json(&ConnectionInfo::WebRtc(WebRtcConnectionInfo {
        path: "direct",
        setup_ms: 12.0,
        signaling_ms: 2.0,
        ice_ms: 8.0,
        data_channels_open_ms: 2.0,
        rtt_us: None,
        local_candidate_type: "host",
        remote_candidate_type: "srflx",
        ice_protocol: "udp",
        addresses_included: false,
        local_addr: None,
        remote_addr: None,
    }));
    assert_eq!(json["transport"], "webrtc");
    assert_eq!(json["path"], "direct");
    assert_eq!(json["addresses_included"], false);
    assert!(json.get("local_addr").is_none());
    assert!(json.get("remote_addr").is_none());
}

#[tokio::test]
async fn rust_webrtc_runs_upload_reverse_parallel_one_and_four() {
    let worker = WorkerdGuard::start(&repo_root()).await;

    for reverse in [false, true] {
        for parallel in [1, 4] {
            let cell_started = std::time::Instant::now();
            eprintln!("starting webrtc matrix reverse={reverse} parallel={parallel}");
            let mut server = start_server(ServerOptions {
                transport: Transport::WebRtc,
                webrtc: Some(signal_options()),
                ..Default::default()
            })
            .await
            .unwrap();
            let server_ready = cell_started.elapsed();
            let code = server
                .endpoint_ticket
                .clone()
                .expect("WebRTC server room code");
            let result = run_client(
                &code,
                ClientOptions {
                    transport: Transport::WebRtc,
                    reverse,
                    parallel,
                    duration: 1,
                    len: Some(16 * 1_024),
                    interval: None,
                    webrtc: Some(signal_options()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap_or_else(|error| {
                panic!("webrtc matrix reverse={reverse} parallel={parallel} failed: {error}")
            });
            let client_done = cell_started.elapsed();

            assert_eq!(result.reverse, reverse);
            assert_eq!(result.local.streams.len(), parallel as usize);
            assert!(result.sent_bytes.saturating_add(result.received_bytes) > 0);
            assert!(
                result.send_bits_per_second.is_finite()
                    && result.receive_bits_per_second.is_finite()
            );
            let local_bytes = result
                .local
                .streams
                .iter()
                .map(|stream| stream.bytes)
                .sum::<u64>();
            let remote_bytes = result
                .remote
                .streams
                .iter()
                .map(|stream| stream.bytes)
                .sum::<u64>();
            let maximum = local_bytes.max(remote_bytes).max(1);
            assert!(
                local_bytes.abs_diff(remote_bytes) as f64 / maximum as f64 <= 0.02,
                "application byte drift exceeded 2%: local={local_bytes} remote={remote_bytes}"
            );
            match result.connection.as_ref().expect("WebRTC diagnostics") {
                ConnectionInfo::WebRtc(info) => assert_eq!(info.path, "direct"),
                #[cfg(any(feature = "iroh", feature = "quic"))]
                other => panic!("unexpected diagnostics: {other:?}"),
            };
            tokio::time::timeout(Duration::from_secs(3), server.wait_terminal())
                .await
                .expect("WebRTC server terminal outcome must be observable")
                .expect("WebRTC has a terminal lifecycle")
                .expect("completed WebRTC server session succeeds");
            server.close().await;
            eprintln!(
                "webrtc matrix reverse={reverse} parallel={parallel}: server_ready={server_ready:?} client_done={client_done:?} total={:?}",
                cell_started.elapsed()
            );
        }
    }

    let idle_server = start_server(ServerOptions {
        transport: Transport::WebRtc,
        webrtc: Some(signal_options()),
        ..Default::default()
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), idle_server.close())
        .await
        .expect("cancelling a server waiting for a WebRTC peer must be bounded");

    let dropped_peer_server = start_server(ServerOptions {
        transport: Transport::WebRtc,
        webrtc: Some(signal_options()),
        ..Default::default()
    })
    .await
    .unwrap();
    let code = dropped_peer_server
        .endpoint_ticket
        .clone()
        .expect("WebRTC server room code");
    let options = signal_options();
    let client = SignalingClient::new(options.clone(), None);
    let mut signaling = client.join(&code).await.unwrap();
    let mut dropped_peer = negotiate_offerer(&options, &mut signaling).await.unwrap();
    dropped_peer.peer.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), dropped_peer_server.close())
        .await
        .expect("abrupt WebRTC peer loss must release the server promptly");

    worker.shutdown().await;
}
