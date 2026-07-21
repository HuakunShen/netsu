mod common;

use std::process::Stdio;
use tokio::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_netsu")
}

fn cli_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_for_tcp_server(port: u16) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("TCP server did not become ready");
}

#[tokio::test]
async fn server_and_client_run_a_tcp_test_end_to_end() {
    let port = cli_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for_tcp_server(port).await;

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
    let port = cli_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for_tcp_server(port).await;

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
    let port = cli_port(); // nothing listening
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

#[cfg(not(feature = "quic"))]
#[tokio::test]
async fn quic_flag_is_recognized_and_reports_missing_feature() {
    let out = Command::new(bin())
        .args(["client", "127.0.0.1", "--quic", "--quic-insecure"])
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("quic support not compiled in; rebuild with --features quic")
    );
}

#[tokio::test]
async fn sigint_during_an_active_test_frees_the_port() {
    let port = cli_port();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for_tcp_server(port).await;

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

#[cfg(feature = "quic")]
#[tokio::test]
async fn quic_cli_rejects_invalid_flag_combinations_before_network_io() {
    let cases = [
        (vec!["client", "127.0.0.1", "--quic"], "trust mode"),
        (
            vec![
                "client",
                "127.0.0.1",
                "--quic",
                "--quic-ca",
                "ca.pem",
                "--quic-insecure",
            ],
            "exactly one",
        ),
        (vec!["server", "--quic"], "certificate mode"),
        (
            vec![
                "server",
                "--quic",
                "--quic-self-signed",
                "--quic-cert",
                "cert.pem",
                "--quic-key",
                "key.pem",
            ],
            "exactly one",
        ),
        (
            vec!["client", "127.0.0.1", "--quic", "--ws", "--quic-insecure"],
            "mutually exclusive",
        ),
        (
            vec!["client", "127.0.0.1", "--quic", "--quic-insecure", "-u"],
            "--udp",
        ),
    ];

    for (args, expected) in cases {
        let out = Command::new(bin()).args(&args).output().await.unwrap();
        assert!(!out.status.success(), "expected failure for {args:?}");
        assert!(out.stdout.is_empty(), "stdout not empty for {args:?}");
        assert!(!out.stderr.is_empty(), "stderr empty for {args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "stderr for {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(feature = "quic")]
#[tokio::test]
async fn quic_cli_json_upload_and_reverse_are_pure_and_diagnostic() {
    let port = cli_port();
    let port_text = port.to_string();
    let mut server = Command::new(bin())
        .args(["server", "-p", &port_text, "--quic", "--quic-self-signed"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    for reverse in [false, true] {
        let mut args = vec![
            "client",
            "127.0.0.1",
            "-p",
            &port_text,
            "-t",
            "1",
            "--quic",
            "--quic-insecure",
            "--json",
        ];
        if reverse {
            args.push("-R");
        }
        let out = Command::new(bin()).args(args).output().await.unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("warning"));
        assert!(stderr.contains("--quic-insecure"));
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout must be pure JSON");
        assert_eq!(json["connection"]["transport"], "quic");
        assert_eq!(json["connection"]["path"], "direct");
        assert!(json["connection"]["handshake_ms"].as_f64().unwrap() >= 0.0);
        assert!(json["end"]["sum_sent"]["bytes"].as_u64().unwrap() > 0);
        assert!(json["end"]["sum_received"]["bytes"].as_u64().unwrap() > 0);
    }

    let _ = server.kill().await;
}
