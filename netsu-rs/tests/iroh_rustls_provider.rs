#![cfg(feature = "iroh")]

use std::time::Duration;

use netsu::p2p::rendezkey;
use tokio::net::TcpListener;

/// Rendez-key can be the first TLS user in an iroh-only process. Keep this in
/// its own integration-test binary so another transport cannot install the
/// process-wide rustls provider first and hide the regression.
#[tokio::test]
async fn iroh_rendezkey_can_initialize_https_without_prior_transport() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local TLS sink");
    let port = listener
        .local_addr()
        .expect("local TLS sink address")
        .port();

    let sink = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept HTTPS client");
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    let client = tokio::spawn(async move {
        let _ = rendezkey::claim(&format!("https://127.0.0.1:{port}"), "test").await;
    });

    let outcome = tokio::time::timeout(Duration::from_secs(5), client)
        .await
        .expect("HTTPS initialization timed out");
    sink.abort();

    assert!(
        outcome.is_ok(),
        "HTTPS initialization panicked instead of returning a TLS error: {outcome:?}"
    );
}
