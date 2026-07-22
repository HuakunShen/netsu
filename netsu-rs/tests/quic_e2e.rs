#![cfg(feature = "quic")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use netsu::client::{ClientOptions, QuicClientOptions, Transport, run_client};
use netsu::error::{NetsuError, SetupPhase};
use netsu::protocol::cookie::{cookie_to_bytes, make_cookie};
use netsu::protocol::framing::{MAX_JSON, read_json, read_state, write_json, write_state};
use netsu::protocol::params::{self, TestParams};
use netsu::protocol::results::{self, EndResults, StreamResult};
use netsu::protocol::states::{
    COOKIE_SIZE, CREATE_STREAMS, DISPLAY_RESULTS, EXCHANGE_RESULTS, IPERF_DONE, PARAM_EXCHANGE,
    TEST_END, TEST_RUNNING, TEST_START,
};
use netsu::server::{NetsuServer, QuicServerOptions, ServerOptions, start_server};
use netsu::streams::channel::DataChannel;
use netsu::streams::runner::{
    SharedChannel, SharedCounters, StreamCounters, next_stream_id, run_receiver, run_sender,
};
use netsu::transport::quic::channel::{QuicChannel, QuicPipe};
use netsu::transport::quic::endpoint::{QuicEndpoint, STREAMS_TIMEOUT};
use netsu::transport::quic::tls::{client_config, server_config};
use tokio::sync::{Mutex, watch};

struct FakeServerObservation {
    control_cookie: [u8; COOKIE_SIZE],
    data_streams: usize,
    client_results: EndResults,
}

async fn spawn_fake_server(
    reverse: bool,
    parallel: u32,
) -> (
    u16,
    tokio::task::JoinHandle<Result<FakeServerObservation, NetsuError>>,
) {
    let (config, _) = server_config(&QuicServerOptions {
        self_signed: true,
        cert_path: None,
        key_path: None,
    })
    .unwrap();
    let endpoint = QuicEndpoint::bind_server("127.0.0.1:0".parse().unwrap(), config).unwrap();
    let port = endpoint.local_addr().unwrap().port();

    let task = tokio::spawn(async move {
        let (connection, _) = endpoint.accept().await?;
        let (send, recv) = tokio::time::timeout(STREAMS_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| NetsuError::Timeout)?
            .map_err(|error| NetsuError::Protocol(error.to_string()))?;
        let mut control = QuicPipe::new(send, recv);
        let cookie_bytes = control
            .read_exact(COOKIE_SIZE, Some(Duration::from_secs(2)))
            .await?;
        let control_cookie: [u8; COOKIE_SIZE] = cookie_bytes.try_into().unwrap();
        assert_eq!(control_cookie[COOKIE_SIZE - 1], 0);

        assert!(
            tokio::time::timeout(Duration::from_millis(100), connection.accept_bi())
                .await
                .is_err(),
            "client opened a data stream before CREATE_STREAMS"
        );

        write_state(&mut control, PARAM_EXCHANGE).await?;
        let params_json: serde_json::Value =
            read_json(&mut control, MAX_JSON, Some(Duration::from_secs(2))).await?;
        let test_params = params::decode(params_json)?;
        assert_eq!(test_params.reverse, reverse);
        assert_eq!(test_params.parallel, parallel);

        assert!(
            tokio::time::timeout(Duration::from_millis(100), connection.accept_bi())
                .await
                .is_err(),
            "client opened a data stream before CREATE_STREAMS"
        );
        write_state(&mut control, CREATE_STREAMS).await?;

        let mut streams = Vec::with_capacity(parallel as usize);
        for index in 0..parallel as usize {
            let (send, mut recv) = tokio::time::timeout(STREAMS_TIMEOUT, connection.accept_bi())
                .await
                .map_err(|_| NetsuError::Timeout)?
                .map_err(|error| NetsuError::Protocol(error.to_string()))?;
            let mut data_cookie = [0u8; COOKIE_SIZE];
            recv.read_exact(&mut data_cookie)
                .await
                .map_err(|error| NetsuError::Protocol(error.to_string()))?;
            assert_eq!(data_cookie, control_cookie, "data cookie {index} differs");
            let channel: SharedChannel = Arc::new(Mutex::new(
                Box::new(QuicChannel::new(send, recv)) as Box<dyn DataChannel>,
            ));
            let counters: SharedCounters =
                Arc::new(Mutex::new(StreamCounters::new(next_stream_id(index))));
            streams.push((channel, counters));
        }

        write_state(&mut control, TEST_START).await?;
        let (stop, stop_rx) = watch::channel(false);
        let meter = Arc::new(Mutex::new(netsu::stats::IntervalMeter::new(Instant::now())));
        let mut tasks = Vec::with_capacity(streams.len());
        for (channel, counters) in &streams {
            tasks.push(if reverse {
                tokio::spawn(run_sender(
                    channel.clone(),
                    counters.clone(),
                    meter.clone(),
                    32 * 1024,
                    stop_rx.clone(),
                ))
            } else {
                tokio::spawn(run_receiver(
                    channel.clone(),
                    counters.clone(),
                    meter.clone(),
                    stop_rx.clone(),
                ))
            });
        }
        write_state(&mut control, TEST_RUNNING).await?;
        let state = read_state(&mut control, Some(Duration::from_secs(5))).await?;
        assert_eq!(state, TEST_END);

        let _ = stop.send(true);
        for task in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
        for (channel, _) in &streams {
            channel.lock().await.close().await;
        }

        write_state(&mut control, EXCHANGE_RESULTS).await?;
        let client_json: serde_json::Value =
            read_json(&mut control, MAX_JSON, Some(Duration::from_secs(2))).await?;
        let client_results = results::decode(client_json)?;

        let mut stream_results = Vec::with_capacity(streams.len());
        for (_, counters) in &streams {
            let counters = counters.lock().await;
            stream_results.push(StreamResult {
                id: counters.id,
                bytes: counters.bytes,
                retransmits: -1,
                jitter: 0.0,
                errors: 0,
                packets: 0,
                start_time: 0.0,
                end_time: 1.0,
            });
        }
        let server_results = EndResults {
            sender_has_retransmits: if reverse { 0 } else { -1 },
            streams: stream_results,
        };
        write_json(&mut control, &results::encode(&server_results)).await?;
        write_state(&mut control, DISPLAY_RESULTS).await?;
        assert_eq!(
            read_state(&mut control, Some(Duration::from_secs(2))).await?,
            IPERF_DONE
        );
        control.close().await;
        connection.close(0u32.into(), b"fake server complete");
        endpoint.close().await;

        Ok(FakeServerObservation {
            control_cookie,
            data_streams: streams.len(),
            client_results,
        })
    });
    (port, task)
}

fn quic_client_options(port: u16, reverse: bool, parallel: u32) -> ClientOptions {
    ClientOptions {
        port,
        transport: Transport::Quic,
        reverse,
        duration: 1,
        parallel,
        quic: Some(QuicClientOptions {
            insecure: true,
            ca_path: None,
        }),
        ..Default::default()
    }
}

async fn start_quic_test_server() -> NetsuServer {
    start_server(ServerOptions {
        port: 0,
        transport: Transport::Quic,
        quic: Some(QuicServerOptions {
            self_signed: true,
            cert_path: None,
            key_path: None,
        }),
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn raw_quic_connection(port: u16) -> (QuicEndpoint, quinn::Connection) {
    let config = client_config(&QuicClientOptions {
        insecure: true,
        ca_path: None,
    })
    .unwrap();
    let endpoint = QuicEndpoint::bind_client(config).unwrap();
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let (connection, _) = endpoint.connect(address, "localhost").await.unwrap();
    (endpoint, connection)
}

async fn run_server_case(reverse: bool, parallel: u32) {
    let server = start_quic_test_server().await;
    let result = run_client(
        "127.0.0.1",
        quic_client_options(server.port, reverse, parallel),
        None,
    )
    .await
    .unwrap();
    assert!(result.sent_bytes > 0);
    assert!(result.received_bytes > 0);
    assert_eq!(result.local.streams.len(), parallel as usize);
    assert!(result.connection.is_some());
    server.close().await;
}

#[tokio::test]
async fn client_quic_waits_for_create_streams_and_opens_exact_parallel_count() {
    let (port, server) = spawn_fake_server(false, 4).await;
    let result = run_client("127.0.0.1", quic_client_options(port, false, 4), None)
        .await
        .unwrap();
    let observation = server.await.unwrap().unwrap();

    assert_eq!(observation.data_streams, 4);
    assert_eq!(observation.control_cookie[COOKIE_SIZE - 1], 0);
    assert_eq!(observation.client_results.streams.len(), 4);
    assert!(result.sent_bytes > 0);
    assert!(result.received_bytes > 0);
    assert_eq!(result.local.streams.len(), 4);
    assert!(result.connection.is_some());
}

#[tokio::test]
async fn client_quic_reverse_reads_payload_from_receive_halves() {
    let (port, server) = spawn_fake_server(true, 1).await;
    let result = run_client("127.0.0.1", quic_client_options(port, true, 1), None)
        .await
        .unwrap();
    server.await.unwrap().unwrap();

    assert!(result.sent_bytes > 0);
    assert!(result.received_bytes > 0);
    assert_eq!(result.local.streams.len(), 1);
}

#[tokio::test]
async fn client_quic_unreachable_server_errors_within_twelve_seconds() {
    let reserved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);

    let started = Instant::now();
    let error = run_client("127.0.0.1", quic_client_options(port, false, 1), None)
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(12));
    assert!(matches!(
        error,
        NetsuError::Setup {
            transport: "quic",
            phase: SetupPhase::QuicHandshake,
            ..
        }
    ));
}

#[tokio::test]
async fn client_quic_rejects_udp_before_network_io() {
    let mut options = quic_client_options(9, false, 1);
    options.udp = true;
    let error = run_client("127.0.0.1", options, None).await.unwrap_err();
    assert!(error.to_string().contains("UDP"));
}

#[tokio::test]
async fn server_quic_four_cell_direction_parallel_matrix() {
    for (reverse, parallel) in [(false, 1), (true, 1), (false, 4), (true, 4)] {
        run_server_case(reverse, parallel).await;
    }
}

#[tokio::test]
async fn server_quic_accepts_two_sequential_tests() {
    let server = start_quic_test_server().await;
    for reverse in [false, true] {
        let result = run_client(
            "127.0.0.1",
            quic_client_options(server.port, reverse, 1),
            None,
        )
        .await
        .unwrap();
        assert!(result.sent_bytes > 0);
        assert!(result.received_bytes > 0);
    }
    server.close().await;
}

#[tokio::test]
async fn server_quic_rejects_concurrent_test_as_busy() {
    let server = start_quic_test_server().await;
    let running = Arc::new(AtomicBool::new(false));
    let running_reporter = running.clone();
    let mut first_options = quic_client_options(server.port, false, 1);
    first_options.duration = 2;
    first_options.interval = Some(Duration::from_millis(50));
    let first = tokio::spawn(run_client(
        "127.0.0.1",
        first_options,
        Some(Box::new(move |_| {
            running_reporter.store(true, Ordering::Release);
        })),
    ));

    tokio::time::timeout(Duration::from_secs(2), async {
        while !running.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first client entered TEST_RUNNING");

    let second = run_client(
        "127.0.0.1",
        quic_client_options(server.port, false, 1),
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(second, NetsuError::ServerBusy));
    assert!(first.await.unwrap().is_ok());
    server.close().await;
}

#[tokio::test]
async fn server_quic_malformed_cookie_does_not_poison_next_test() {
    let server = start_quic_test_server().await;
    let (endpoint, connection) = raw_quic_connection(server.port).await;
    let (mut send, _receive) = connection.open_bi().await.unwrap();
    send.write_all(b"short").await.unwrap();
    send.finish().unwrap();
    connection.close(0u32.into(), b"malformed cookie test");
    endpoint.close().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = run_client(
        "127.0.0.1",
        quic_client_options(server.port, false, 1),
        None,
    )
    .await
    .unwrap();
    assert!(result.received_bytes > 0);
    server.close().await;
}

#[tokio::test]
async fn server_quic_extra_data_stream_closes_connection_and_recovers() {
    let server = start_quic_test_server().await;
    let (endpoint, connection) = raw_quic_connection(server.port).await;
    let cookie = cookie_to_bytes(&make_cookie());
    let (send, receive) = connection.open_bi().await.unwrap();
    let mut control = QuicPipe::new(send, receive);
    control.write_all(&cookie).await.unwrap();
    assert_eq!(
        read_state(&mut control, Some(Duration::from_secs(2)))
            .await
            .unwrap(),
        PARAM_EXCHANGE
    );
    let params = TestParams {
        udp: false,
        time: 1,
        parallel: 1,
        len: 32 * 1024,
        reverse: false,
        bandwidth: 0,
    };
    write_json(&mut control, &params::encode(&params))
        .await
        .unwrap();
    assert_eq!(
        read_state(&mut control, Some(Duration::from_secs(2)))
            .await
            .unwrap(),
        CREATE_STREAMS
    );
    let (mut first_send, _first_receive) = connection.open_bi().await.unwrap();
    first_send.write_all(&cookie).await.unwrap();
    let (mut extra_send, _extra_receive) = connection.open_bi().await.unwrap();
    extra_send.write_all(&cookie).await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), connection.closed())
        .await
        .expect("extra stream must close the QUIC connection");
    endpoint.close().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let healthy = run_client(
        "127.0.0.1",
        quic_client_options(server.port, false, 1),
        None,
    )
    .await
    .unwrap();
    assert!(healthy.received_bytes > 0);
    server.close().await;
}

#[tokio::test]
async fn server_quic_abrupt_disconnect_releases_active_slot() {
    let server = start_quic_test_server().await;
    let (endpoint, connection) = raw_quic_connection(server.port).await;
    let (send, receive) = connection.open_bi().await.unwrap();
    let mut control = QuicPipe::new(send, receive);
    control
        .write_all(&cookie_to_bytes(&make_cookie()))
        .await
        .unwrap();
    connection.close(0u32.into(), b"abrupt disconnect");
    endpoint.close().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let healthy = run_client(
        "127.0.0.1",
        quic_client_options(server.port, false, 1),
        None,
    )
    .await
    .unwrap();
    assert!(healthy.received_bytes > 0);
    server.close().await;
}

#[tokio::test]
async fn server_quic_close_is_bounded_during_incomplete_setup() {
    let server = start_quic_test_server().await;
    let (endpoint, connection) = raw_quic_connection(server.port).await;

    tokio::time::timeout(Duration::from_secs(3), server.close())
        .await
        .expect("server close exceeded its endpoint shutdown bound");
    connection.close(0u32.into(), b"test complete");
    endpoint.close().await;
}
