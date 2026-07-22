//! End-to-end: netsu's iperf3 control state machine driven over one iroh/QUIC
//! connection. The server binds a direct-only iroh endpoint and hands out a
//! ticket; the client dials it. Same assertions shape as `rs_to_rs`.
#![cfg(feature = "iroh")]

use netsu::client::{ClientOptions, Transport, run_client};
use netsu::protocol::params::DEFAULT_TCP_LEN;
use netsu::server::{ServerOptions, start_server};

#[tokio::test]
async fn iroh_matrix_reverse_and_parallel() {
    for reverse in [false, true] {
        for parallel in [1u32, 3] {
            let server = start_server(ServerOptions {
                transport: Transport::Iroh,
                direct_only: true,
                ..Default::default()
            })
            .await
            .unwrap();
            let ticket = server
                .endpoint_ticket
                .clone()
                .expect("iroh server exposes a ticket");

            let r = run_client(
                &ticket,
                ClientOptions {
                    transport: Transport::Iroh,
                    direct_only: true,
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
                "reverse={reverse} parallel={parallel}: sent={}",
                r.sent_bytes
            );
            assert!(
                r.received_bytes > 0,
                "reverse={reverse} parallel={parallel}"
            );
            // QUIC is reliable, but shutdown may cancel one in-flight write per
            // stream after the peer has received a prefix and before the local
            // sender counter is updated. Bound that accounting race to exactly
            // one configured block per stream.
            let in_flight_allowance = DEFAULT_TCP_LEN as u64 * parallel as u64;
            assert!(
                r.received_bytes <= r.sent_bytes + in_flight_allowance,
                "reverse={reverse} parallel={parallel}: sent={} received={}",
                r.sent_bytes,
                r.received_bytes
            );
            assert_eq!(
                r.local.streams.len(),
                parallel as usize,
                "reverse={reverse} parallel={parallel}"
            );
            // The result carries the observed path; direct-only must be direct.
            let conn = r
                .connection
                .as_ref()
                .expect("iroh result carries a connection block");
            let conn = match conn {
                netsu::client::ConnectionInfo::Iroh(conn) => conn,
                #[cfg(feature = "quic")]
                netsu::client::ConnectionInfo::Quic(_) => {
                    panic!("expected iroh connection diagnostics")
                }
                #[cfg(feature = "webrtc")]
                netsu::client::ConnectionInfo::WebRtc(_) => {
                    panic!("expected iroh connection diagnostics")
                }
            };
            assert_eq!(
                conn.observed_path, "direct",
                "reverse={reverse} parallel={parallel}: path={}",
                conn.observed_path
            );

            server.close().await;
        }
    }
}

#[tokio::test]
async fn iroh_serves_a_second_test_after_the_first() {
    let server = start_server(ServerOptions {
        transport: Transport::Iroh,
        direct_only: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let ticket = server.endpoint_ticket.clone().expect("ticket");

    for _ in 0..2 {
        run_client(
            &ticket,
            ClientOptions {
                transport: Transport::Iroh,
                direct_only: true,
                duration: 1,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    }

    server.close().await;
}
