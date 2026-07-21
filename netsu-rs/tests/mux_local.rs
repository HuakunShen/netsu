//! End-to-end mux engine over an in-process iroh pair: the headline
//! "input latency under file load" experiment, plus the custom scenario.
#![cfg(feature = "iroh")]

use std::time::Duration;

use netsu::mux::config::{RunConfig, ScenarioName, StreamSpec, WorkloadKind};
use netsu::mux::protocol::MUX_ALPN;
use netsu::mux::{receiver, runner};
use netsu::p2p::endpoint::LocalPair;

#[tokio::test]
async fn input_file_measures_probe_latency_under_load() {
    let pair = LocalPair::connect(MUX_ALPN).await.unwrap();
    let server_conn = pair.server_connection.clone();
    let serve = tokio::spawn(async move { receiver::serve(server_conn).await });

    let config = RunConfig {
        scenario: ScenarioName::InputFile,
        duration: Duration::from_secs(2),
        warmup: Duration::from_millis(200),
        cooldown: Duration::from_millis(100),
        ..Default::default()
    };
    let outcome = runner::run(&pair.client_connection, &config).await.unwrap();

    // One measured Input probe, one File load stream.
    let input = outcome
        .streams
        .iter()
        .find(|s| s.kind == WorkloadKind::Input)
        .expect("input stream present");
    let file = outcome
        .streams
        .iter()
        .find(|s| s.kind == WorkloadKind::File)
        .expect("file stream present");

    assert!(input.measured);
    let lat = input.latency.as_ref().expect("input has latency");
    assert!(
        lat.count > 0,
        "expected some measured input RTTs, got {}",
        lat.count
    );
    assert!(lat.p50_us > 0);

    assert!(!file.measured);
    assert!(
        file.bytes_sent > 1_000_000,
        "file should saturate: {}",
        file.bytes_sent
    );
    assert!(file.bytes_received > 0);

    serve.await.unwrap().unwrap();
    pair.close().await;
}

#[tokio::test]
async fn custom_scenario_runs_probe_and_load_streams() {
    let pair = LocalPair::connect(MUX_ALPN).await.unwrap();
    let server_conn = pair.server_connection.clone();
    let serve = tokio::spawn(async move { receiver::serve(server_conn).await });

    let config = RunConfig {
        scenario: ScenarioName::Custom,
        duration: Duration::from_secs(1),
        warmup: Duration::from_millis(100),
        cooldown: Duration::from_millis(100),
        custom_streams: vec![
            StreamSpec::parse("prio=30,hz=200,payload=64,deadline=50ms").unwrap(),
            StreamSpec::parse("prio=0,saturating").unwrap(),
        ],
        ..Default::default()
    };
    let outcome = runner::run(&pair.client_connection, &config).await.unwrap();

    assert_eq!(outcome.streams.len(), 2);
    let probe = outcome.streams.iter().find(|s| s.measured).unwrap();
    assert_eq!(probe.priority, 30);
    assert!(probe.latency.as_ref().unwrap().count > 0);
    let load = outcome.streams.iter().find(|s| !s.measured).unwrap();
    assert_eq!(load.priority, 0);
    assert!(load.bytes_sent > 0 && load.bytes_received > 0);

    serve.await.unwrap().unwrap();
    pair.close().await;
}
