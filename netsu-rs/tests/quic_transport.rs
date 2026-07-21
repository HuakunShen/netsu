#![cfg(feature = "quic")]

use std::path::PathBuf;

use netsu::client::{ClientOptions, QuicClientOptions, Transport};
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
