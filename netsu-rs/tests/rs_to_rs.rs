mod common;

use common::next_port;
use netsu::client::{ClientOptions, run_client};
use netsu::server::{ServerOptions, start_server};

#[tokio::test]
async fn tcp_matrix_reverse_and_parallel() {
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            let port = next_port();
            let server = start_server(ServerOptions {
                port,
                ..Default::default()
            })
            .await
            .unwrap();

            let r = run_client(
                "127.0.0.1",
                ClientOptions {
                    port,
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
            assert!(r.received_bytes > 0);
            assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
            assert_eq!(r.local.streams.len(), parallel as usize);

            server.close().await;
        }
    }
}

#[tokio::test]
async fn serves_a_second_test_after_the_first_finishes() {
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 1,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    let again = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 1,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert!(again.sent_bytes > 0);

    server.close().await;
}

#[tokio::test]
async fn rejects_a_concurrent_client_with_access_denied() {
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    let first = tokio::spawn(run_client(
        "127.0.0.1",
        ClientOptions {
            port,
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
            duration: 1,
            ..Default::default()
        },
        None,
    )
    .await;
    assert!(matches!(second, Err(netsu::error::NetsuError::ServerBusy)));

    first.await.unwrap().unwrap();
    server.close().await;
}

#[tokio::test]
async fn rejects_a_requested_time_over_the_server_cap() {
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        max_test_seconds: 5,
        ..Default::default()
    })
    .await
    .unwrap();

    let got = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 60,
            ..Default::default()
        },
        None,
    )
    .await;
    assert!(got.is_err());

    server.close().await;
}

#[tokio::test]
async fn malformed_control_input_does_not_wedge_the_server() {
    use tokio::io::AsyncWriteExt;
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    // Connect, send a valid-looking cookie, then garbage where params JSON belongs.
    {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        sock.write_all(&[b'a'; 37]).await.unwrap();
        sock.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).await.unwrap(); // absurd length prefix
        sock.shutdown().await.ok();
    }

    // The server must return to idle and serve a real test.
    let r = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 1,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert!(r.sent_bytes > 0);

    server.close().await;
}
