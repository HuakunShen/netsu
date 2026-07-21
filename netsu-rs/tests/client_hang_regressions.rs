//! Regression tests for two client hangs a code reviewer reproduced against a
//! fake, misbehaving server (no real iperf3 needed — these adversarial peer
//! behaviors don't occur against real iperf3 or the TS server, both of which
//! close their data socket immediately after TEST_END and before
//! EXCHANGE_RESULTS; that's exactly why the bugs slipped through iperf3-based
//! testing). Both scenarios are reverse mode, since the client's receiver
//! task holding the shared channel mutex across a pending, unbounded
//! `read_chunk().await` is the common root cause: any control-loop code that
//! locks the same channel blocks until the peer sends bytes or closes.
//!
//! - `reverse_mode_survives_idle_data_socket_at_exchange_results`: the fake
//!   server goes silent on TEST_END (never closes the data socket) and then
//!   drives EXCHANGE_RESULTS anyway. Previously wedged in
//!   `handle_exchange_results`'s live-read fallback.
//! - `server_error_mid_test_returns_promptly_instead_of_hanging_teardown`:
//!   the fake server sends SERVER_ERROR mid-test, so `run_loop` returns
//!   `Err` without ever reaching EXCHANGE_RESULTS. Previously wedged in
//!   `teardown` -> `StreamState::close` -> `self.channel.lock().await`,
//!   which made the required SERVER_ERROR -> `NetsuError::ServerError`
//!   mapping unobservable.

use netsu::client::{ClientOptions, run_client};
use netsu::error::NetsuError;
use netsu::protocol::framing::{MAX_JSON, read_json, write_json};
use netsu::protocol::results::{EndResults, StreamResult, encode as encode_results};
use netsu::protocol::states::{
    COOKIE_SIZE, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS, PARAM_EXCHANGE, SERVER_ERROR,
    TEST_RUNNING, TEST_START,
};
use netsu::transport::tcp::TcpPipe;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::Duration;

/// Accepts the control connection plus `parallel` data connections, reading
/// each one's cookie (the minimal handshake `client.rs`'s `open_stream` and
/// `open_tcp_stream` expect), and drives the control channel through
/// PARAM_EXCHANGE / CREATE_STREAMS / TEST_START / TEST_RUNNING.
async fn accept_and_handshake(
    listener: &TcpListener,
    parallel: u32,
) -> (TcpPipe, Vec<tokio::net::TcpStream>) {
    let (control_sock, _) = listener.accept().await.unwrap();
    let mut control = TcpPipe::from_stream(control_sock);
    control.read_exact(COOKIE_SIZE, None).await.unwrap();

    netsu::protocol::framing::write_state(&mut control, PARAM_EXCHANGE)
        .await
        .unwrap();
    let _params: serde_json::Value = read_json(&mut control, MAX_JSON, None).await.unwrap();

    netsu::protocol::framing::write_state(&mut control, CREATE_STREAMS)
        .await
        .unwrap();
    let mut data_socks = Vec::new();
    for _ in 0..parallel {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut cookie_buf = [0u8; COOKIE_SIZE];
        sock.read_exact(&mut cookie_buf).await.unwrap();
        data_socks.push(sock);
    }

    netsu::protocol::framing::write_state(&mut control, TEST_START)
        .await
        .unwrap();
    netsu::protocol::framing::write_state(&mut control, TEST_RUNNING)
        .await
        .unwrap();

    (control, data_socks)
}

fn one_stream_results(bytes: u64) -> EndResults {
    EndResults {
        sender_has_retransmits: 0,
        streams: vec![StreamResult {
            id: 1,
            bytes,
            retransmits: -1,
            jitter: 0.0,
            errors: 0,
            packets: 0,
            start_time: 0.0,
            end_time: 1.0,
        }],
    }
}

/// Reviewer's first hang: full reverse handshake, some data, then silence on
/// TEST_END with the data socket kept open, then EXCHANGE_RESULTS anyway.
async fn fake_server_idle_socket_then_exchange_results(listener: TcpListener) {
    let (mut control, mut data_socks) = accept_and_handshake(&listener, 1).await;

    // Act as the reverse-mode sender briefly, then go idle: never close the
    // data socket, never send more.
    for sock in &mut data_socks {
        let _ = sock.write_all(&[0xABu8; 65536]).await;
    }

    // Read (and thus consume off the wire) the TEST_END the client's own
    // short duration timer will send unprompted, then go silent: per
    // reverse-mode protocol fact 3, the client leaves the data socket open
    // at this point, waiting for us to stop sending on our own — we never
    // do, and we never close it either.
    let state = netsu::protocol::framing::read_state(&mut control, None)
        .await
        .unwrap();
    assert_eq!(state, netsu::protocol::states::TEST_END);

    netsu::protocol::framing::write_state(&mut control, EXCHANGE_RESULTS)
        .await
        .unwrap();
    let _local: serde_json::Value = read_json(&mut control, MAX_JSON, None).await.unwrap();
    write_json(&mut control, &encode_results(&one_stream_results(65536)))
        .await
        .unwrap();
    netsu::protocol::framing::write_state(&mut control, DISPLAY_RESULTS)
        .await
        .unwrap();

    // Keep the data socket alive a little longer so the client's teardown
    // sees a clean close rather than an already-reset peer.
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(data_socks);
}

#[tokio::test]
async fn reverse_mode_survives_idle_data_socket_at_exchange_results() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(fake_server_idle_socket_then_exchange_results(listener));

    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        run_client(
            "127.0.0.1",
            ClientOptions {
                port,
                duration: 1,
                reverse: true,
                ..Default::default()
            },
            None,
        ),
    )
    .await;

    let result = outcome.expect(
        "run_client hung past the 10s guard — regression of the reverse-mode \
         EXCHANGE_RESULTS live-read hang on an idle data socket",
    );
    assert!(result.is_ok(), "expected a clean result, got {result:?}");
    server.await.unwrap();
}

/// Reviewer's second hang: SERVER_ERROR mid-test, before EXCHANGE_RESULTS,
/// with the data socket left idle — isolates the teardown -> close hang.
/// Crucially, the peer must never give the client's stuck receiver task an
/// EOF (or close at all) within the test's guard window below: if it did,
/// that alone would unblock a pre-fix receiver and mask the bug.
async fn fake_server_sends_server_error_mid_test(listener: TcpListener) {
    let (mut control, data_socks) = accept_and_handshake(&listener, 1).await;

    // Leave the data socket fully idle: never write, never close.
    tokio::time::sleep(Duration::from_millis(300)).await;

    netsu::protocol::framing::write_state(&mut control, SERVER_ERROR)
        .await
        .unwrap();

    // Hold everything open well past the client-side guard timeout below —
    // the whole point of this scenario is that nothing on the peer side
    // ever gives the client's stuck receiver task a way to unblock on its
    // own. The test aborts this task once it has its answer, so this long
    // sleep is never actually waited out.
    tokio::time::sleep(Duration::from_secs(20)).await;
    drop(data_socks);
}

#[tokio::test]
async fn server_error_mid_test_returns_promptly_instead_of_hanging_teardown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(fake_server_sends_server_error_mid_test(listener));

    // duration is long so the client's own duration timer cannot race ahead
    // of the server's mid-test SERVER_ERROR.
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        run_client(
            "127.0.0.1",
            ClientOptions {
                port,
                duration: 30,
                reverse: true,
                ..Default::default()
            },
            None,
        ),
    )
    .await;

    // Whether or not run_client hung, we already have our answer: stop the
    // fake server's 20s sleep instead of waiting it out.
    server.abort();

    let result = outcome.expect(
        "run_client hung past the 10s guard — regression of the reverse-mode \
         teardown hang (StreamState::close awaiting the channel mutex forever)",
    );
    match result {
        Err(NetsuError::ServerError) => {}
        other => panic!("expected Err(NetsuError::ServerError), got {other:?}"),
    }
}
