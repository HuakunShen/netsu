mod common;

use common::{has_iperf3, next_port, spawn_iperf3_server};
use netsu::client::{ClientOptions, run_client};
use netsu::server::{ServerOptions, start_server};
use std::process::Stdio;
use tokio::process::Command;

async fn run_iperf3_client(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new("iperf3")
        .args(args)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn iperf3");
    let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "iperf3 output not json: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (out.status.code().unwrap_or(-1), json)
}

#[tokio::test]
async fn netsu_client_to_iperf3_server_udp() {
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
            udp: true,
            bandwidth: Some(5_000_000),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    let u = r.udp_stats.expect("udp stats");
    assert!(u.packets > 100, "packets {}", u.packets);
    assert!(u.lost_percent < 10.0, "lost {}%", u.lost_percent);
    let _ = server.kill().await;
}

#[tokio::test]
async fn netsu_client_reverse_from_iperf3_server_udp() {
    // The gap the TS suite still has: netsu as UDP *receiver* from official iperf3.
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
            udp: true,
            reverse: true,
            bandwidth: Some(5_000_000),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();

    let u = r.udp_stats.expect("udp stats");
    assert!(u.packets > 100, "packets {}", u.packets);
    assert!(u.lost_percent < 10.0, "lost {}%", u.lost_percent);
    let _ = server.kill().await;
}

#[tokio::test]
async fn iperf3_client_to_netsu_server_udp() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    let (code, json) = run_iperf3_client(&[
        "-c",
        "127.0.0.1",
        "-p",
        &port.to_string(),
        "-t",
        "2",
        "-u",
        "-b",
        "5M",
        "-l",
        "1460",
    ])
    .await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["packets"].as_u64().unwrap() > 100);
    assert!(json["end"]["sum"]["lost_percent"].as_f64().unwrap() < 10.0);

    server.close().await;
}

#[tokio::test]
async fn iperf3_reverse_client_to_netsu_server_udp_unpinned_blocksize() {
    // No -l: iperf3 negotiates blksize from path MTU (16332 on loopback).
    // This is the case that exposed the send-capability bug in Phase 1.
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    let (code, json) = run_iperf3_client(&[
        "-c",
        "127.0.0.1",
        "-p",
        &port.to_string(),
        "-t",
        "2",
        "-u",
        "-b",
        "5M",
        "-R",
    ])
    .await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["bytes"].as_u64().unwrap() > 100_000);
    assert!(json["end"]["sum"]["lost_percent"].as_f64().unwrap() < 10.0);

    server.close().await;
}

#[tokio::test]
async fn unpaced_reverse_udp_does_not_livelock_the_server() {
    // Phase 1's Critical: `iperf3 -u -b 0 -R` makes the server the *unpaced*
    // UDP sender. If `Pacer::gate` returned without yielding on the unpaced
    // path (as the TS version once did), the send loop would spin at ~99% CPU,
    // never read the control channel's TEST_END, and wedge the server. This
    // asserts the test both completes within a bound (no livelock) AND that the
    // server serves a subsequent test afterward (not wedged).
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    let port_s = port.to_string();
    let args = [
        "-c",
        "127.0.0.1",
        "-p",
        &port_s,
        "-t",
        "2",
        "-u",
        "-b", //
        "0",  // unlimited: the server sends unpaced — the livelock trigger
        "-R",
        "-l",
        "1460",
    ];
    let (code, json) =
        tokio::time::timeout(std::time::Duration::from_secs(20), run_iperf3_client(&args))
            .await
            .expect("unpaced reverse UDP test did not complete — server livelocked");
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["bytes"].as_u64().unwrap() > 0);

    // The server must still serve a normal test — proof it wasn't wedged.
    let r = run_client(
        "127.0.0.1",
        ClientOptions {
            port,
            duration: 1,
            udp: true,
            bandwidth: Some(5_000_000),
            ..Default::default()
        },
        None,
    )
    .await
    .expect("server wedged after the unpaced test");
    assert!(r.udp_stats.expect("udp stats").packets > 0);

    server.close().await;
}

#[tokio::test]
async fn iperf3_parallel_udp_streams_to_netsu_server() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let port = next_port();
    let server = start_server(ServerOptions {
        port,
        ..Default::default()
    })
    .await
    .unwrap();

    let (code, json) = run_iperf3_client(&[
        "-c",
        "127.0.0.1",
        "-p",
        &port.to_string(),
        "-t",
        "2",
        "-u",
        "-b",
        "5M",
        "-l",
        "1460",
        "-P",
        "4",
    ])
    .await;
    assert_eq!(code, 0, "iperf3 failed: {json}");

    server.close().await;
}

#[tokio::test]
async fn udp_rs_to_rs_matrix() {
    // Includes parallel, the coverage the TS suite lacks netsu-to-netsu.
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
                    udp: true,
                    reverse,
                    parallel,
                    bandwidth: Some(5_000_000),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("reverse={reverse} parallel={parallel}: {e}"));

            let u = r.udp_stats.expect("udp stats");
            assert!(u.packets > 0);
            assert!(
                u.lost_percent < 10.0,
                "reverse={reverse} parallel={parallel} lost {}%",
                u.lost_percent
            );
            assert_eq!(r.local.streams.len(), parallel as usize);

            server.close().await;
        }
    }
}
