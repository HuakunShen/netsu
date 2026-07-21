mod common;

use common::{has_iperf3, next_port};
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
async fn iperf3_upload_completes_and_reports_bytes() {
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

    let (code, json) =
        run_iperf3_client(&["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2"]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum_sent"]["bytes"].as_u64().unwrap() > 1_000_000);
    assert!(json["end"]["sum_received"]["bytes"].as_u64().unwrap() > 0);

    server.close().await;
}

#[tokio::test]
async fn iperf3_reverse_receives_from_netsu_server() {
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

    let (code, json) =
        run_iperf3_client(&["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-R"]).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum_received"]["bytes"].as_u64().unwrap() > 1_000_000);

    server.close().await;
}

#[tokio::test]
async fn iperf3_parallel_two_streams() {
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
        "-P",
        "2",
    ])
    .await;
    assert_eq!(code, 0, "iperf3 failed: {json}");

    server.close().await;
}
