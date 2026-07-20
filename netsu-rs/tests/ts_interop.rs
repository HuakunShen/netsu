mod common;

use common::next_port;
use netsu::client::{ClientOptions, run_client};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

fn ts_package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../packages/netsu")
}

fn ts_cli_built() -> bool {
    ts_package_dir().join("dist/cli.mjs").exists()
}

/// Start the TypeScript netsu server via its built CLI.
async fn spawn_ts_server(port: u16) -> std::io::Result<Child> {
    let mut child = Command::new("node")
        .arg(ts_package_dir().join("dist/cli.mjs"))
        .args(["server", "-p", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Ok(Some(l)) = lines.next_line().await {
            if l.contains("listening") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !ready {
        let _ = child.kill().await;
        return Err(std::io::Error::other("ts server did not start"));
    }
    Ok(child)
}

#[tokio::test]
async fn rust_client_against_typescript_server_tcp() {
    if !ts_cli_built() {
        eprintln!("skipping: packages/netsu/dist/cli.mjs not built (run `bun run build` there)");
        return;
    }
    let port = next_port();
    let mut server = spawn_ts_server(port).await.unwrap();

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

    assert!(r.sent_bytes > 1_000_000);
    assert!(r.received_bytes as f64 <= r.sent_bytes as f64 * 1.01);
    let _ = server.kill().await;
}

#[tokio::test]
async fn rust_client_reverse_against_typescript_server_tcp() {
    if !ts_cli_built() {
        eprintln!("skipping: packages/netsu/dist/cli.mjs not built");
        return;
    }
    let port = next_port();
    let mut server = spawn_ts_server(port).await.unwrap();

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

    assert!(r.received_bytes > 1_000_000);
    let _ = server.kill().await;
}
