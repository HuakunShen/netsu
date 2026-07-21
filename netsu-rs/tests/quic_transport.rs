#![cfg(feature = "quic")]

use std::path::PathBuf;

use netsu::client::{
    ClientOptions, ConnectionInfo, QuicClientOptions, QuicConnectionInfo, Transport,
    connection_json,
};
use netsu::error::{NetsuError, SetupPhase};
use netsu::server::{QuicServerOptions, ServerOptions};

#[test]
fn quic_client_requires_exactly_one_trust_mode() {
    let missing = ClientOptions {
        transport: Transport::Quic,
        ..Default::default()
    };
    assert!(
        missing
            .validate()
            .unwrap_err()
            .to_string()
            .contains("QUIC client options")
    );

    let both = ClientOptions {
        transport: Transport::Quic,
        quic: Some(QuicClientOptions {
            insecure: true,
            ca_path: Some(PathBuf::from("ca.pem")),
        }),
        ..Default::default()
    };
    assert!(
        both.validate()
            .unwrap_err()
            .to_string()
            .contains("exactly one")
    );
}

#[test]
fn quic_server_requires_self_signed_or_cert_key_pair() {
    let missing = ServerOptions {
        transport: Transport::Quic,
        ..Default::default()
    };
    assert!(missing.validate().is_err());

    let half_pair = ServerOptions {
        transport: Transport::Quic,
        quic: Some(QuicServerOptions {
            self_signed: false,
            cert_path: Some(PathBuf::from("cert.pem")),
            key_path: None,
        }),
        ..Default::default()
    };
    assert!(
        half_pair
            .validate()
            .unwrap_err()
            .to_string()
            .contains("certificate and key")
    );
}

#[test]
fn quic_alpn_is_versioned_and_namespaced() {
    assert_eq!(netsu::transport::quic::QUIC_ALPN, b"netsu/iperf3-quic/1");
}

#[test]
fn quic_connection_json_is_stable_and_redacted() {
    let info = ConnectionInfo::Quic(QuicConnectionInfo {
        handshake_ms: 12.5,
        rtt_us: Some(2_500),
        remote_addr: Some("203.0.113.7:5201".into()),
        certificate_verification: "ca",
        lost_packets: Some(3),
        congestion_events: Some(1),
    });

    let json = connection_json(&info);
    assert_eq!(json["transport"], "quic");
    assert_eq!(json["path"], "direct");
    assert_eq!(json["certificate_verification"], "ca");
    assert_eq!(json["handshake_ms"], 12.5);
    assert!(json["handshake_ms"].as_f64().unwrap().is_finite());
    assert!(json.get("remote_addr").is_none());
    assert!(json.get("private_key").is_none());
}

#[cfg(feature = "iroh")]
#[test]
fn normalized_iroh_json_preserves_existing_keys() {
    use netsu::client::IrohConnectionInfo;

    let json = connection_json(&ConnectionInfo::Iroh(IrohConnectionInfo {
        observed_path: "direct".into(),
        rtt_us: Some(1_250),
        remote_addr: Some("198.51.100.4:4433".into()),
    }));

    assert_eq!(json["transport"], "iroh");
    assert_eq!(json["observed_path"], "direct");
    assert_eq!(json["rtt_us"], 1_250);
    assert_eq!(json["remote_addr"], "198.51.100.4:4433");
}

#[test]
fn setup_errors_name_transport_and_phase() {
    let error = NetsuError::Setup {
        transport: "quic",
        phase: SetupPhase::Tls,
        detail: "certificate rejected".into(),
    };

    assert_eq!(
        error.to_string(),
        "quic setup failed during tls: certificate rejected"
    );
}
