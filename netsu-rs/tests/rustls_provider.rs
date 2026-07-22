#![cfg(all(feature = "quic", feature = "webrtc"))]

use std::time::Duration;

use netsu::transport::webrtc::config::WebRtcOptions;
use netsu::transport::webrtc::signaling::SignalingClient;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;

/// Regression for the TUI WebRTC-host crash. This integration test has its own
/// process, so no other test can pre-install a process-wide rustls provider and
/// accidentally hide ambiguous Cargo features.
#[tokio::test]
async fn quic_and_webrtc_can_initialize_wss_in_one_process() {
    let options = WebRtcOptions::new(
        "https://rendez-key.xc.huakun.tech/v1/signal",
        ["stun:stun.cloudflare.com:3478"],
        false,
    )
    .expect("valid public signaling configuration");
    let _signaling = SignalingClient::new(options, None);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local TLS sink");
    let port = listener
        .local_addr()
        .expect("local TLS sink address")
        .port();

    let sink = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept WSS client");
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    let client = tokio::spawn(async move {
        let _ = connect_async(format!("wss://127.0.0.1:{port}/v1/signal/rooms/test/ws")).await;
    });

    let outcome = tokio::time::timeout(Duration::from_secs(5), client)
        .await
        .expect("WSS initialization timed out");
    sink.abort();

    assert!(
        outcome.is_ok(),
        "WSS initialization panicked instead of returning a TLS error: {outcome:?}"
    );
}
