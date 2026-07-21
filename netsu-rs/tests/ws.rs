#![cfg(feature = "ws")]

mod common;

use common::next_port;
use netsu::client::{ClientOptions, Transport, run_client};
use netsu::server::{ServerOptions, start_server};

#[tokio::test]
async fn ws_matrix_reverse_and_parallel() {
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            let port = next_port();
            let server = start_server(ServerOptions {
                port,
                transport: Transport::Ws,
                ..Default::default()
            })
            .await
            .unwrap();

            let r = run_client(
                "127.0.0.1",
                ClientOptions {
                    port,
                    transport: Transport::Ws,
                    duration: 1,
                    reverse,
                    parallel,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("reverse={reverse} parallel={parallel}: {e}"));

            assert!(
                r.sent_bytes > 100_000,
                "reverse={reverse} parallel={parallel}"
            );
            assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
            assert_eq!(r.local.streams.len(), parallel as usize);

            server.close().await;
        }
    }
}

#[tokio::test]
async fn ws_server_enforces_the_single_test_lock() {
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        transport: Transport::Ws,
        ..Default::default()
    })
    .await
    .unwrap();

    let first = tokio::spawn(run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            transport: Transport::Ws,
            duration: 2,
            ..Default::default()
        },
        None,
    ));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let second = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            transport: Transport::Ws,
            duration: 1,
            ..Default::default()
        },
        None,
    )
    .await;
    assert!(second.is_err());

    first.await.unwrap().unwrap();
    server.close().await;
}

#[tokio::test]
async fn ws_connect_times_out_against_a_non_upgrading_peer() {
    // A plain TCP listener that accepts but never answers the HTTP upgrade.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        // Hold the connection open, answer nothing.
        std::mem::forget(sock);
    });

    let start = std::time::Instant::now();
    let got = netsu::transport::ws::WsPipe::connect(
        "127.0.0.1",
        port,
        std::time::Duration::from_millis(500),
    )
    .await;
    assert!(got.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}
