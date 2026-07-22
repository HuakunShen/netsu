#![cfg(feature = "webrtc")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use netsu::client::Transport;
use netsu::error::{SetupPhase, WebRtcSetupFailure, webrtc_setup_error};
use netsu::transport::webrtc::config::WebRtcOptions;
use netsu::transport::webrtc::signaling::{
    ClientSignalMessage, DescriptionType, SIGNAL_EXCHANGE_TIMEOUT, SIGNAL_PEER_WAIT_TIMEOUT,
    ServerSignalMessage, SignalRole, decode_server_message, encode_client_message,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const CLI_WORKER_PORT: u16 = 18_789;

struct WorkerdGuard {
    child: Child,
}

impl WorkerdGuard {
    async fn start(repo_root: &Path) -> Self {
        let mut child = Command::new(repo_root.join("scripts/dev-webrtc-signal.sh"))
            .current_dir(repo_root)
            .env("SIGNAL_WORKER_PORT", CLI_WORKER_PORT.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start Wrangler signaling wrapper");
        let stdout = child.stdout.take().expect("wrapper stdout");
        // Wrangler/workerd startup can be dominated by a cold Node module
        // load when this workspace lives on an external disk. Wait for the
        // wrapper's real health-checked READY line without making that I/O a
        // protocol-test failure.
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
            format!("READY http://127.0.0.1:{CLI_WORKER_PORT}/v1/signal")
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

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_netsu")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("netsu-rs lives under the repository root")
        .to_path_buf()
}

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn webrtc_configuration_is_direct_only_and_rejects_turn() {
    let options = WebRtcOptions::new(
        "https://signal.example/v1/signal",
        ["stun:stun.example:3478"],
        false,
    )
    .expect("valid direct-only WebRTC options");

    assert_eq!(options.signal_url.scheme(), "https");
    assert_eq!(options.stun_urls, ["stun:stun.example:3478"]);
    assert!(!options.include_addresses);
    assert_eq!(Transport::WebRtc, Transport::WebRtc);

    for url in [
        "turn:relay.example:3478",
        "turns:relay.example:5349",
        "https://not-an-ice-server.example",
        "",
    ] {
        let error = WebRtcOptions::new("https://signal.example/v1/signal", [url], false)
            .expect_err("non-STUN ICE URL must be rejected");
        assert!(error.to_string().contains("STUN"));
    }
}

#[test]
fn webrtc_configuration_rejects_bad_signal_urls_and_too_many_stun_urls() {
    for url in ["ws://signal.example", "ftp://signal.example", "not a URL"] {
        assert!(
            WebRtcOptions::new(url, std::iter::empty::<&str>(), false).is_err(),
            "accepted {url}"
        );
    }

    let error = WebRtcOptions::new(
        "http://127.0.0.1:8787/v1/signal",
        [
            "stun:a.example:3478",
            "stun:b.example:3478",
            "stun:c.example:3478",
            "stun:d.example:3478",
            "stun:e.example:3478",
        ],
        false,
    )
    .expect_err("more than four STUN servers must be rejected");
    assert!(error.to_string().contains("at most 4"));
}

#[test]
fn signaling_base_url_appends_room_routes_without_dropping_the_base_path() {
    let options = WebRtcOptions::new(
        "https://signal.example/v1/signal",
        std::iter::empty::<&str>(),
        false,
    )
    .unwrap();
    assert_eq!(
        options.rooms_url().unwrap().as_str(),
        "https://signal.example/v1/signal/rooms"
    );
    assert_eq!(
        options.room_websocket_url("ABCD-EFGH").unwrap().as_str(),
        "wss://signal.example/v1/signal/rooms/ABCD-EFGH/ws"
    );
}

#[test]
fn signaling_v1_messages_match_the_worker_wire_contract() {
    let bind = ClientSignalMessage::bind_listener("secret-fixture");
    assert!(!format!("{bind:?}").contains("secret-fixture"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encode_client_message(&bind).unwrap()).unwrap(),
        serde_json::json!({
            "v": 1,
            "type": "bind",
            "role": "listener",
            "secret": "secret-fixture",
        })
    );

    let offer = ClientSignalMessage::description(DescriptionType::Offer, "fixture-offer");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encode_client_message(&offer).unwrap()).unwrap(),
        serde_json::json!({
            "v": 1,
            "type": "description",
            "sdp_type": "offer",
            "sdp": "fixture-offer",
        })
    );

    let bound =
        decode_server_message(r#"{"v":1,"type":"bound","role":"joiner","expires_in_seconds":60}"#)
            .unwrap();
    assert_eq!(
        bound,
        ServerSignalMessage::Bound {
            v: 1,
            role: SignalRole::Joiner,
            expires_in_seconds: 60,
        }
    );
}

#[test]
fn signaling_decoder_enforces_version_and_resource_limits() {
    let wrong_version = decode_server_message(r#"{"v":2,"type":"peer_ready"}"#)
        .expect_err("v2 must not be accepted by a v1 client");
    assert!(wrong_version.to_string().contains("version"));

    let oversized = format!(
        r#"{{"v":1,"type":"description","sdp_type":"answer","sdp":"{}"}}"#,
        "x".repeat(65_536)
    );
    let error = decode_server_message(&oversized).expect_err("oversized frame must fail");
    assert!(error.to_string().contains("64 KiB"));
}

#[test]
fn setup_phases_cover_the_full_webrtc_handshake() {
    let phases = [
        SetupPhase::SignalingConnect,
        SetupPhase::SignalingRoom,
        SetupPhase::OfferAnswer,
        SetupPhase::IceGathering,
        SetupPhase::IceConnected,
        SetupPhase::PeerConnected,
        SetupPhase::ChannelsOpen,
    ];
    assert_eq!(phases.len(), 7);
}

#[test]
fn listener_wait_window_outlives_the_offer_answer_deadline() {
    assert!(SIGNAL_PEER_WAIT_TIMEOUT > SIGNAL_EXCHANGE_TIMEOUT);
    assert_eq!(SIGNAL_PEER_WAIT_TIMEOUT.as_secs(), 600);
}

#[test]
fn webrtc_setup_errors_use_a_sanitized_public_vocabulary() {
    let error = webrtc_setup_error(
        SetupPhase::OfferAnswer,
        WebRtcSetupFailure::InvalidRemoteDescription,
    );
    let display = error.to_string();
    assert_eq!(
        display,
        "webrtc setup failed during offer/answer: remote description was rejected"
    );
    for secret_material in ["candidate:", "v=0", "listener_secret", "192.0.2.1"] {
        assert!(!display.contains(secret_material));
    }
}

#[tokio::test]
async fn webrtc_help_exposes_only_direct_mode_configuration() {
    for command in ["server", "client"] {
        let output = Command::new(bin())
            .args([command, "--help"])
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout).to_lowercase();
        for flag in ["--webrtc", "--signal-url", "--stun", "--include-addresses"] {
            assert!(help.contains(flag), "{command} help missing {flag}: {help}");
        }
        assert!(
            !help.contains("--turn"),
            "{command} unexpectedly exposes TURN"
        );
        assert!(
            !help.contains("--relay"),
            "{command} unexpectedly exposes relay"
        );
    }
}

#[tokio::test]
async fn webrtc_cli_rejects_invalid_configuration_with_exit_two() {
    let signal = "http://127.0.0.1:8787/v1/signal";
    let cases = [
        (vec!["client", "ABCD-EFGH", "--webrtc"], "--signal-url"),
        (
            vec![
                "client",
                "ABCD-EFGH",
                "--webrtc",
                "--signal-url",
                signal,
                "--stun",
                "turn:relay.example:3478",
            ],
            "STUN",
        ),
        (
            vec![
                "client",
                "ABCD-EFGH",
                "--webrtc",
                "--signal-url",
                signal,
                "--udp",
            ],
            "mutually exclusive",
        ),
        (
            vec![
                "client",
                "ABCD-EFGH",
                "--webrtc",
                "--signal-url",
                signal,
                "--ws",
            ],
            "mutually exclusive",
        ),
        (
            vec![
                "client",
                "ABCD-EFGH",
                "--webrtc",
                "--signal-url",
                signal,
                "--iroh",
            ],
            "mutually exclusive",
        ),
        (
            vec![
                "client",
                "ABCD-EFGH",
                "--webrtc",
                "--signal-url",
                signal,
                "--quic",
            ],
            "mutually exclusive",
        ),
        (
            vec!["server", "--webrtc", "--signal-url", signal, "--relay"],
            "unexpected argument '--relay'",
        ),
        (vec!["server", "--signal-url", signal], "require --webrtc"),
    ];

    for (args, expected) in cases {
        let output = Command::new(bin()).args(&args).output().await.unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "wrong exit for {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "stdout not empty for {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "stderr for {args:?} did not contain {expected:?}: {stderr}"
        );
    }
}

#[tokio::test]
async fn webrtc_json_setup_failure_is_structured_and_has_no_throughput() {
    let signal = format!("http://127.0.0.1:{}/v1/signal", unused_port());
    let output = Command::new(bin())
        .args([
            "client",
            "ABCD-EFGH",
            "--webrtc",
            "--signal-url",
            &signal,
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be pure JSON");
    assert_eq!(value["error"]["transport"], "webrtc");
    assert_eq!(value["error"]["kind"], "setup_failed");
    assert!(value["error"]["phase"].is_string());
    assert!(value["error"]["message"].is_string());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("bits_per_second"),
        "failed setup must not look like a zero-throughput benchmark"
    );
}

#[tokio::test]
async fn webrtc_cli_server_and_client_complete_over_real_workerd() {
    let worker = WorkerdGuard::start(&repo_root()).await;
    let signal_url = format!("http://127.0.0.1:{CLI_WORKER_PORT}/v1/signal");
    let mut server = Command::new(bin())
        .args(["server", "--webrtc", "--signal-url", &signal_url])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn WebRTC CLI server");
    let stdout = server.stdout.take().expect("server stdout");
    let mut lines = BufReader::new(stdout).lines();
    // The all-feature debug binary has the same cold-load cost as Wrangler on
    // external workspaces; readiness is the emitted room code.
    let code = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(line) = lines.next_line().await.expect("read server output") {
            if let Some(code) = line.strip_prefix("code: ") {
                return code.to_string();
            }
        }
        panic!("WebRTC CLI server exited before printing a room code");
    })
    .await
    .expect("WebRTC CLI server did not print a room code");

    let output = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new(bin())
            .args([
                "client",
                &code,
                "--webrtc",
                "--signal-url",
                &signal_url,
                "-t",
                "1",
                "-P",
                "4",
                "--json",
            ])
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .expect("WebRTC CLI client timed out")
    .expect("run WebRTC CLI client");
    assert!(
        output.status.success(),
        "WebRTC CLI client failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("client stdout must be pure JSON");
    assert_eq!(value["connection"]["transport"], "webrtc");
    assert_eq!(value["connection"]["path"], "direct");
    assert_eq!(value["connection"]["streams"], 4);
    assert!(
        value["end"]["sum_sent"]["bits_per_second"]
            .as_f64()
            .is_some_and(|rate| rate.is_finite() && rate > 0.0)
    );

    if let Some(pid) = server.id() {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    let status = tokio::time::timeout(Duration::from_secs(5), server.wait())
        .await
        .expect("WebRTC CLI server did not stop")
        .expect("wait for WebRTC CLI server");
    assert!(status.success());
    worker.shutdown().await;
}
