mod common;

use common::{
    UDP_SETUP_ATTEMPTS, has_iperf3, next_port, retry_iperf3_to_netsu, retry_udp_setup,
    run_iperf3_bounded, spawn_iperf3_server,
};
use netsu::client::{ClientOptions, run_client};
use netsu::server::{ServerOptions, start_server};
use std::time::Duration;

/// Build `iperf3 -c 127.0.0.1 -p <port> …` arg vectors for the port a fresh
/// server bound to this attempt (see `common::retry_iperf3_to_netsu`). `extra`
/// carries the per-test flags (`-R`, `-P 4`, blocksize, …).
fn iperf3_udp_args(port: u16, extra: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "2", "-u"]
        .into_iter()
        .map(String::from)
        .collect();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

#[tokio::test]
async fn netsu_client_to_iperf3_server_udp() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    // Retry the whole attempt (fresh one-shot `iperf3 -s -1` on a fresh port each
    // time) on the pre-measurement UDP setup transient — the netsu client sends
    // its hello once with no retransmit, so a dropped loopback setup datagram
    // would otherwise fail before any packets flow (see the harness in `common`).
    let r = retry_udp_setup(|| async {
        let port = next_port();
        // `?` folds a slow `iperf3 -s` startup (an io error) into the retry too —
        // it's another pre-measurement hiccup, not a test failure.
        let mut server = spawn_iperf3_server(port, &[]).await?;
        let res = run_client(
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
        .await;
        let _ = server.kill().await;
        res
    })
    .await
    .unwrap();

    let u = r.udp_stats.expect("udp stats");
    assert!(u.packets > 100, "packets {}", u.packets);
    assert!(u.lost_percent < 10.0, "lost {}%", u.lost_percent);
}

#[tokio::test]
async fn netsu_client_reverse_from_iperf3_server_udp() {
    // The gap the TS suite still has: netsu as UDP *receiver* from official iperf3.
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    // Retry the whole attempt (fresh one-shot server on a fresh port each time)
    // on the pre-measurement UDP setup transient — see `netsu_client_to_iperf3_server_udp`.
    let r = retry_udp_setup(|| async {
        let port = next_port();
        let mut server = spawn_iperf3_server(port, &[]).await?;
        let res = run_client(
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
        .await;
        let _ = server.kill().await;
        res
    })
    .await
    .unwrap();

    let u = r.udp_stats.expect("udp stats");
    assert!(u.packets > 100, "packets {}", u.packets);
    assert!(u.lost_percent < 10.0, "lost {}%", u.lost_percent);
}

#[tokio::test]
async fn iperf3_client_to_netsu_server_udp() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    // Each attempt binds a fresh netsu server and runs iperf3 against it,
    // retrying the pre-measurement setup transient (a dropped one-shot hello —
    // see `common::retry_iperf3_to_netsu`).
    let (code, json) =
        retry_iperf3_to_netsu(|port| iperf3_udp_args(port, &["-b", "5M", "-l", "1460"])).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["packets"].as_u64().unwrap() > 100);
    assert!(json["end"]["sum"]["lost_percent"].as_f64().unwrap() < 10.0);
}

#[tokio::test]
async fn iperf3_reverse_client_to_netsu_server_udp_unpinned_blocksize() {
    // No -l: iperf3 negotiates blksize from path MTU (16332 on loopback).
    // This is the case that exposed the send-capability bug in Phase 1.
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let (code, json) =
        retry_iperf3_to_netsu(|port| iperf3_udp_args(port, &["-b", "5M", "-R"])).await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
    assert!(json["end"]["sum"]["bytes"].as_u64().unwrap() > 100_000);
    assert!(json["end"]["sum"]["lost_percent"].as_f64().unwrap() < 10.0);
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
    // `-b 0`: the server sends unpaced — the livelock trigger. `-R -l 1460`.
    let unpaced = |port: u16| iperf3_udp_args(port, &["-b", "0", "-R", "-l", "1460"]);

    // The 20s bound is the real assertion: a livelocked server never completes.
    // But a dropped one-shot hello *also* stalls past 20s (the server's accept
    // hangs), and that is a setup flake, not a livelock — so an overrun (or a
    // non-zero iperf3 exit, or a dropped follow-up hello) retries the whole
    // scenario on a fresh server, and only a failure on the *final* attempt is
    // reported. A genuine livelock reproduces every attempt and still fails.
    let mut served = false;
    for attempt in 1..=UDP_SETUP_ATTEMPTS {
        let retry = attempt < UDP_SETUP_ATTEMPTS;
        let port = next_port();
        let server = start_server(ServerOptions {
            port,
            ..Default::default()
        })
        .await
        .unwrap();

        match run_iperf3_bounded(&unpaced(port), Duration::from_secs(20)).await {
            // Overran the bound: a livelock *or* a dropped hello, indistinguishable.
            None => {
                server.close().await;
                if retry {
                    continue;
                }
                panic!("unpaced reverse UDP test did not complete — server livelocked");
            }
            Some((code, json)) => {
                if code != 0 {
                    server.close().await;
                    if retry {
                        continue;
                    }
                    panic!("iperf3 failed: {json}");
                }
                assert!(json["end"]["sum"]["bytes"].as_u64().unwrap() > 0);

                // The server must still serve a normal test — proof it wasn't
                // wedged. Reuses this attempt's server on purpose (that is the
                // assertion); a dropped follow-up hello retries the whole scenario.
                let follow_up = run_client(
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
                .await;
                server.close().await;
                match follow_up {
                    Ok(r) => {
                        assert!(r.udp_stats.expect("udp stats").packets > 0);
                        served = true;
                        break;
                    }
                    // A follow-up setup flake (any pre-measurement error) retries
                    // the whole scenario; only the final attempt's failure sticks.
                    Err(_) if retry => continue,
                    Err(e) => panic!("server wedged after the unpaced test: {e}"),
                }
            }
        }
    }
    assert!(
        served,
        "unpaced reverse UDP test never completed a clean attempt"
    );
}

#[tokio::test]
async fn iperf3_parallel_udp_streams_to_netsu_server() {
    if !has_iperf3() {
        eprintln!("skipping: no iperf3");
        return;
    }
    let (code, json) =
        retry_iperf3_to_netsu(|port| iperf3_udp_args(port, &["-b", "5M", "-l", "1460", "-P", "4"]))
            .await;
    assert_eq!(code, 0, "iperf3 failed: {json}");
}

#[tokio::test]
async fn reverse_udp_with_sub_header_len_is_refused_not_panicked() {
    // A peer choosing `len` below the 12-byte UDP header (params allows len >= 4)
    // must not reach the server's sender and panic its task with a silent
    // 0-byte result. The server refuses at PARAM_EXCHANGE, so the client sees a
    // clean error and the server stays healthy enough to serve a real test.
    // Each attempt uses one fresh server for BOTH halves (refuse, then serve) —
    // the "same server stays healthy" intent is preserved within an attempt.
    // The refusal is deterministic (PARAM_EXCHANGE, before any UDP setup), so it
    // is asserted directly; only the follow-up test's setup can flake, and its
    // `?` feeds the harness's retry (see the module note in `common`).
    retry_udp_setup(|| async {
        let port = next_port();
        let server = start_server(ServerOptions {
            port,
            ..Default::default()
        })
        .await?;

        let got = run_client(
            "127.0.0.1",
            ClientOptions {
                port,
                duration: 1,
                udp: true,
                reverse: true,
                len: Some(8),
                bandwidth: Some(5_000_000),
                ..Default::default()
            },
            None,
        )
        .await;
        assert!(got.is_err(), "expected refusal, got {got:?}");

        // The server must still serve a normal test — proof it wasn't wedged.
        let follow_up = run_client(
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
        .await;
        server.close().await;
        let r = follow_up?;
        assert!(r.udp_stats.expect("udp stats").packets > 0);
        Ok(())
    })
    .await
    .expect("server wedged after the refused test");
}

#[tokio::test]
async fn udp_reports_nonzero_per_interval_throughput() {
    // Regression: the UDP sender/receiver loops must feed the interval meter,
    // or the live `[SUM]` lines and `--json` intervals[] read 0 bytes/0 bps for
    // every UDP test (while the final summary, from `counters`, stays correct).
    // Covers both directions — forward exercises the sender's meter, reverse the
    // receiver's.
    for reverse in [false, true] {
        // Retry only the pre-measurement UDP setup timeout (a dropped setup
        // datagram, see `common::UDP_SETUP_ATTEMPTS`), rebuilding server + client
        // on a fresh port each attempt (a dropped hello wedges the previous
        // server's slot). The per-interval callback is consumed by each attempt,
        // so a fresh sink is wired every time; the reports of the *successful*
        // attempt are collected inside the closure and returned for the asserts.
        let got: Vec<f64> = retry_udp_setup(|| async {
            let port = next_port();
            let server = start_server(ServerOptions {
                port,
                ..Default::default()
            })
            .await?;
            let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = reports.clone();
            let res = run_client(
                "127.0.0.1",
                ClientOptions {
                    port,
                    duration: 3,
                    udp: true,
                    reverse,
                    bandwidth: Some(5_000_000),
                    ..Default::default()
                },
                Some(Box::new(move |rep| {
                    sink.lock().unwrap().push(rep.bits_per_second);
                })),
            )
            .await;
            server.close().await;
            // Snapshot (releasing the lock) before returning.
            res.map(|_| reports.lock().unwrap().clone())
        })
        .await
        .unwrap_or_else(|e| panic!("reverse={reverse}: {e}"));

        assert!(
            got.len() >= 2,
            "reverse={reverse}: got {} interval reports",
            got.len()
        );
        assert!(
            got.iter().all(|&bps| bps > 0.0),
            "reverse={reverse}: a UDP interval reported zero throughput: {got:?}"
        );
    }
}

#[tokio::test]
async fn udp_rs_to_rs_matrix() {
    // Includes parallel, the coverage the TS suite lacks netsu-to-netsu.
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            // Retry only the pre-measurement UDP setup timeout (a dropped one-shot
            // hello; see `common::UDP_SETUP_ATTEMPTS`), rebuilding server + client
            // on a fresh port each attempt. A dropped hello wedges the previous
            // server on its stream-collect wait, so a fresh server — not a reuse —
            // is what makes the retry clean. Every assertion below still runs
            // against a real, fully measured transfer.
            let r = retry_udp_setup(|| async {
                let port = next_port();
                let server = start_server(ServerOptions {
                    port,
                    ..Default::default()
                })
                .await?;
                let res = run_client(
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
                .await;
                server.close().await;
                res
            })
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
        }
    }
}
