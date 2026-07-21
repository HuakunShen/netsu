#![cfg(feature = "quic")]

use std::path::PathBuf;
use std::{fs, net::SocketAddr, time::Duration};

use netsu::client::{
    ClientOptions, ConnectionInfo, QuicClientOptions, QuicConnectionInfo, Transport,
    connection_json,
};
use netsu::error::{NetsuError, SetupPhase};
use netsu::server::{QuicServerOptions, ServerOptions};
use netsu::streams::channel::DataChannel;
use netsu::transport::quic::channel::{QuicChannel, QuicPipe};
use netsu::transport::quic::endpoint::QuicEndpoint;
use netsu::transport::quic::tls::{client_config, server_config};

struct TempPki {
    path: PathBuf,
}

impl TempPki {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "netsu-quic-pki-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&path).expect("create isolated QUIC PKI directory");
        Self { path }
    }

    fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).expect("write test PKI file");
        path
    }
}

impl Drop for TempPki {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

async fn quic_tls_handshake(
    server_config: quinn::ServerConfig,
    client_config: quinn::ClientConfig,
) -> std::result::Result<(), String> {
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server = quinn::Endpoint::server(server_config, bind_addr).map_err(|e| e.to_string())?;
    let server_addr = server.local_addr().map_err(|e| e.to_string())?;
    let mut client = quinn::Endpoint::client(bind_addr).map_err(|e| e.to_string())?;
    client.set_default_client_config(client_config);

    let attempt = tokio::time::timeout(Duration::from_secs(5), async {
        let server_handshake = async {
            server
                .accept()
                .await
                .expect("server endpoint remains open")
                .await
        };
        let client_handshake = async {
            client
                .connect(server_addr, "localhost")
                .map_err(|e| e.to_string())?
                .await
                .map_err(|e| e.to_string())
        };
        let (server_result, client_result) = tokio::join!(server_handshake, client_handshake);
        let client_connection = client_result?;
        let server_connection = server_result.map_err(|e| e.to_string())?;
        client_connection.close(0u32.into(), b"test complete");
        server_connection.close(0u32.into(), b"test complete");
        Ok(())
    })
    .await
    .map_err(|_| "QUIC TLS handshake timed out".to_string())?;

    server.close(0u32.into(), b"test complete");
    client.close(0u32.into(), b"test complete");
    attempt
}

fn generated_ca_and_server(temp: &TempPki) -> (PathBuf, PathBuf, PathBuf) {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };

    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let mut leaf_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    leaf_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    let ca_path = temp.write("ca.pem", ca_cert.pem());
    let cert_path = temp.write("server.pem", leaf_cert.pem());
    let key_path = temp.write("server-key.pem", leaf_key.serialize_pem());
    (ca_path, cert_path, key_path)
}

async fn connected_quic_pair() -> (
    QuicEndpoint,
    QuicEndpoint,
    quinn::Connection,
    quinn::Connection,
) {
    let server_options = QuicServerOptions {
        self_signed: true,
        cert_path: None,
        key_path: None,
    };
    let (server_config, _) = server_config(&server_options).unwrap();
    let client_config = client_config(&QuicClientOptions {
        insecure: true,
        ca_path: None,
    })
    .unwrap();
    let server = QuicEndpoint::bind_server("127.0.0.1:0".parse().unwrap(), server_config)
        .expect("bind QUIC server endpoint");
    let client = QuicEndpoint::bind_client(client_config).expect("bind QUIC client endpoint");
    let server_addr = server.local_addr().unwrap();
    let (server_connection, client_connection) =
        tokio::join!(server.accept(), client.connect(server_addr, "localhost"));
    let (server_connection, _) = server_connection.expect("accept local QUIC connection");
    let (client_connection, _) = client_connection.expect("connect local QUIC endpoint");
    (server, client, server_connection, client_connection)
}

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

#[tokio::test]
async fn generated_self_signed_tls_requires_insecure_client() {
    let temp = TempPki::new();
    let unrelated = rcgen::generate_simple_self_signed(vec!["unrelated.test".into()]).unwrap();
    let unrelated_ca = temp.write("unrelated-ca.pem", unrelated.cert.pem());

    let options = QuicServerOptions {
        self_signed: true,
        cert_path: None,
        key_path: None,
    };
    let (verified_server, metadata) = server_config(&options).unwrap();
    assert!(metadata.generated);
    assert_eq!(metadata.sha256.len(), 64);
    assert!(metadata.sha256.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(metadata.sha256, metadata.sha256.to_ascii_lowercase());

    let verified_client = client_config(&QuicClientOptions {
        insecure: false,
        ca_path: Some(unrelated_ca),
    })
    .unwrap();
    let error = quic_tls_handshake(verified_server, verified_client)
        .await
        .expect_err("an unrelated CA must reject the generated certificate");
    assert!(
        error.contains("certificate") || error.contains("peer"),
        "unexpected handshake error: {error}"
    );

    let (insecure_server, _) = server_config(&options).unwrap();
    let insecure_client = client_config(&QuicClientOptions {
        insecure: true,
        ca_path: None,
    })
    .unwrap();
    quic_tls_handshake(insecure_server, insecure_client)
        .await
        .expect("explicit insecure mode accepts the generated benchmark certificate");
}

#[tokio::test]
async fn generated_test_ca_tls_authenticates_server() {
    let temp = TempPki::new();
    let (ca_path, cert_path, key_path) = generated_ca_and_server(&temp);

    let (server, metadata) = server_config(&QuicServerOptions {
        self_signed: false,
        cert_path: Some(cert_path),
        key_path: Some(key_path),
    })
    .unwrap();
    assert!(!metadata.generated);
    let client = client_config(&QuicClientOptions {
        insecure: false,
        ca_path: Some(ca_path),
    })
    .unwrap();

    quic_tls_handshake(server, client)
        .await
        .expect("the selected test CA authenticates the server");
}

#[test]
fn malformed_tls_pem_is_rejected_before_binding() {
    let temp = TempPki::new();
    let cert_path = temp.write("broken-cert.pem", "not a certificate");
    let key_path = temp.write("broken-key.pem", "not a private key");

    let error = server_config(&QuicServerOptions {
        self_signed: false,
        cert_path: Some(cert_path),
        key_path: Some(key_path),
    })
    .unwrap_err();

    assert!(matches!(
        error,
        NetsuError::Setup {
            transport: "quic",
            phase: SetupPhase::Tls,
            ..
        }
    ));
}

#[test]
fn multiple_tls_private_keys_are_rejected() {
    let temp = TempPki::new();
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_path = temp.write("server.pem", generated.cert.pem());
    let key_pem = generated.signing_key.serialize_pem();
    let key_path = temp.write("server-key.pem", format!("{key_pem}{key_pem}"));

    let error = server_config(&QuicServerOptions {
        self_signed: false,
        cert_path: Some(cert_path),
        key_path: Some(key_path),
    })
    .unwrap_err();

    assert!(matches!(
        error,
        NetsuError::Setup {
            transport: "quic",
            phase: SetupPhase::Tls,
            ..
        }
    ));
    assert!(error.to_string().contains("exactly one private key"));
}

#[tokio::test]
async fn quic_pipe_read_exact_spans_write_boundaries() {
    let (server, client, server_connection, client_connection) = connected_quic_pair().await;
    let reader_connection = server_connection.clone();
    let reader = tokio::spawn(async move {
        let (send, recv) = reader_connection.accept_bi().await.unwrap();
        let mut pipe = QuicPipe::new(send, recv);
        let bytes = pipe.read_exact(5, Some(Duration::from_secs(2))).await?;
        let closed = pipe.read_exact(1, Some(Duration::from_secs(2))).await;
        Ok::<_, NetsuError>((bytes, closed))
    });

    let (send, recv) = client_connection.open_bi().await.unwrap();
    let mut pipe = QuicPipe::new(send, recv);
    pipe.write_all(&[1, 2]).await.unwrap();
    pipe.write_all(&[3, 4, 5]).await.unwrap();
    pipe.close().await;

    let (bytes, closed) = reader.await.unwrap().unwrap();
    assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
    assert!(matches!(closed, Err(NetsuError::PipeClosed)));

    client_connection.close(0u32.into(), b"test complete");
    server_connection.close(0u32.into(), b"test complete");
    client.close().await;
    server.close().await;
}

#[tokio::test]
async fn quic_channel_round_trips_payload_and_reports_eof() {
    const TOTAL: usize = 1024 * 1024;

    let (server, client, server_connection, client_connection) = connected_quic_pair().await;
    let reader_connection = server_connection.clone();
    let reader = tokio::spawn(async move {
        let (send, recv) = reader_connection.accept_bi().await.unwrap();
        let mut channel = QuicChannel::new(send, recv);
        let mut buffer = vec![0u8; 65_536];
        let mut received = 0usize;
        loop {
            let count = channel.read_chunk(&mut buffer).await?;
            if count == 0 {
                break;
            }
            received += count;
        }
        assert!(channel.error().is_none());
        Ok::<_, NetsuError>(received)
    });

    let (send, recv) = client_connection.open_bi().await.unwrap();
    let mut channel = QuicChannel::new(send, recv);
    let payload = vec![0xa5; TOTAL];
    let chunk_sizes = [1_023usize, 16_384, 65_535, 131_072];
    let mut offset = 0usize;
    let mut chunk_index = 0usize;
    while offset < payload.len() {
        let end = (offset + chunk_sizes[chunk_index % chunk_sizes.len()]).min(payload.len());
        channel.write_chunk(&payload[offset..end]).await.unwrap();
        offset = end;
        chunk_index += 1;
    }
    channel.close().await;

    assert_eq!(reader.await.unwrap().unwrap(), TOTAL);

    client_connection.close(0u32.into(), b"test complete");
    server_connection.close(0u32.into(), b"test complete");
    client.close().await;
    server.close().await;
}

#[tokio::test]
async fn connect_to_unused_udp_port_times_out_in_quic_handshake_phase() {
    let reserved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let unused_addr = reserved.local_addr().unwrap();
    drop(reserved);

    let config = client_config(&QuicClientOptions {
        insecure: true,
        ca_path: None,
    })
    .unwrap();
    let endpoint = QuicEndpoint::bind_client(config).unwrap();
    let started = std::time::Instant::now();
    let error = endpoint
        .connect(unused_addr, "localhost")
        .await
        .expect_err("an unused UDP port must not connect");
    assert!(started.elapsed() < Duration::from_secs(12));
    assert!(matches!(
        error,
        NetsuError::Setup {
            transport: "quic",
            phase: SetupPhase::QuicHandshake,
            ..
        }
    ));
    endpoint.close().await;
}
