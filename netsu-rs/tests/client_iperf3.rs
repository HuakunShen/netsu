mod common;

use common::{has_iperf3, next_port, spawn_iperf3_server};
use netsu::client::{ClientOptions, run_client};

#[tokio::test]
async fn upload_transfers_and_exchanges_results() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 2,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    assert!(!r.reverse);
    assert!(r.sent_bytes > 1_000_000, "sent {}", r.sent_bytes);
    assert!(r.received_bytes > 0);
    assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
    assert!(r.send_bits_per_second > 1_000_000.0);
    let _ = server.kill().await;
}

#[tokio::test]
async fn reverse_receives_from_server() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 2,
            reverse: true,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    assert!(
        r.received_bytes > 1_000_000,
        "received {}",
        r.received_bytes
    );
    assert_eq!(r.local.sender_has_retransmits, -1); // we are the receiver
    let _ = server.kill().await;
}

#[tokio::test]
async fn parallel_three_streams_with_per_stream_results() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let r = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 2,
            parallel: 3,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    assert_eq!(r.local.streams.len(), 3);
    assert_eq!(r.remote.streams.len(), 3);
    // The iperf3 id quirk: 1, 3, 4 — not 1, 2, 3.
    let ids: Vec<u32> = r.local.streams.iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![1, 3, 4]);
    for s in &r.local.streams {
        assert!(s.bytes > 0);
    }
    let _ = server.kill().await;
}

#[tokio::test]
async fn reports_intervals_roughly_every_second() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let mut server = spawn_iperf3_server(port, &[]).await.unwrap();

    let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = reports.clone();
    run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 3,
            ..Default::default()
        },
        Some(Box::new(move |rep| {
            sink.lock().unwrap().push(rep.bits_per_second)
        })),
    )
    .await
    .unwrap();

    {
        // Scoped so the guard drops before the `.await` below — holding a
        // `std::sync::MutexGuard` across an await point trips clippy's
        // `await_holding_lock` lint under `-D warnings`.
        let got = reports.lock().unwrap();
        assert!(got.len() >= 2, "got {} interval reports", got.len());
        for bps in got.iter() {
            assert!(*bps > 0.0);
        }
    }
    let _ = server.kill().await;
}
