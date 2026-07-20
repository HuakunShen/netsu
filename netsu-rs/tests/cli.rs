mod common;

use common::next_port;
use std::process::Stdio;
use tokio::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_netsu")
}

#[tokio::test]
async fn server_and_client_run_a_tcp_test_end_to_end() {
    let port = next_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let out = Command::new(bin())
        .args(["client", "127.0.0.1", "-p", &port.to_string(), "-t", "1"])
        .output()
        .await
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("bits/sec"));
    let _ = server.kill().await;
}

#[tokio::test]
async fn json_mode_emits_only_json_on_stdout() {
    let port = next_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let out = Command::new(bin())
        .args([
            "client",
            "127.0.0.1",
            "-p",
            &port.to_string(),
            "-t",
            "2",
            "-i",
            "1",
            "--json",
        ])
        .output()
        .await
        .unwrap();

    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "stderr not empty: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be pure json");
    let _ = server.kill().await;
}

#[tokio::test]
async fn connection_refused_exits_nonzero_with_empty_stdout_under_json() {
    let port = next_port(); // nothing listening
    let out = Command::new(bin())
        .args([
            "client",
            "127.0.0.1",
            "-p",
            &port.to_string(),
            "-t",
            "1",
            "--json",
        ])
        .output()
        .await
        .unwrap();

    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!out.stderr.is_empty());
}

#[tokio::test]
async fn argument_validation_rejects_bad_flags_before_network_io() {
    for args in [
        vec!["client", "127.0.0.1", "-P", "0"],
        vec!["client", "127.0.0.1", "-t", "0"],
        vec!["client", "127.0.0.1", "-b", "fast"],
    ] {
        let out = Command::new(bin()).args(&args).output().await.unwrap();
        assert!(!out.status.success(), "expected failure for {args:?}");
        assert!(out.stdout.is_empty(), "stdout not empty for {args:?}");
    }
}

#[tokio::test]
async fn sigint_during_an_active_test_frees_the_port() {
    let port = next_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let pid = server.id().expect("pid") as i32;
    unsafe { libc::kill(pid, libc::SIGINT) };
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), server.wait())
        .await
        .expect("server must exit on SIGINT")
        .unwrap();
    let _ = status;

    // Port must be immediately rebindable.
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("port still held");
}
