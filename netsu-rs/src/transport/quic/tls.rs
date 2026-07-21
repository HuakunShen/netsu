//! TLS configuration for native QUIC.
//!
//! Certificate verification is bypassed only through
//! [`InsecureBenchmarkVerifier`], and callers can reach that verifier only by
//! explicitly opting into the benchmark-only insecure mode. It must never be
//! selected as a fallback when CA loading or verification fails.

use std::path::Path;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

use super::QUIC_ALPN;
use crate::client::QuicClientOptions;
use crate::error::{NetsuError, Result, SetupPhase};
use crate::server::QuicServerOptions;

/// Public, non-secret description of the selected server identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateMetadata {
    /// Lowercase SHA-256 fingerprint of the leaf certificate DER.
    pub sha256: String,
    /// Whether netsu generated an ephemeral self-signed identity.
    pub generated: bool,
}

fn tls_error(detail: impl Into<String>) -> NetsuError {
    NetsuError::Setup {
        transport: "quic",
        phase: SetupPhase::Tls,
        detail: detail.into(),
    }
}

fn load_certificates(path: &Path, kind: &str) -> Result<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_file_iter(path)
        .map_err(|error| {
            tls_error(format!(
                "failed to open {kind} PEM {}: {error}",
                path.display()
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| {
            tls_error(format!(
                "failed to parse {kind} PEM {}: {error}",
                path.display()
            ))
        })?;
    if certificates.is_empty() {
        return Err(tls_error(format!(
            "{kind} PEM {} contains no certificates",
            path.display()
        )));
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let mut keys = PrivateKeyDer::pem_file_iter(path)
        .map_err(|error| {
            tls_error(format!(
                "failed to open private key PEM {}: {error}",
                path.display()
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| {
            tls_error(format!(
                "failed to parse private key PEM {}: {error}",
                path.display()
            ))
        })?
        .into_iter();
    let key = keys.next().ok_or_else(|| {
        tls_error(format!(
            "private key PEM {} must contain exactly one private key",
            path.display()
        ))
    })?;
    if keys.next().is_some() {
        return Err(tls_error(format!(
            "private key PEM {} must contain exactly one private key",
            path.display()
        )));
    }
    Ok(key)
}

fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    format!("{:x}", Sha256::digest(certificate.as_ref()))
}

fn ring_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Builds a Quinn server configuration from an explicit identity mode.
pub fn server_config(
    options: &QuicServerOptions,
) -> Result<(quinn::ServerConfig, CertificateMetadata)> {
    let (certificates, private_key, generated) = if options.self_signed {
        if options.cert_path.is_some() || options.key_path.is_some() {
            return Err(tls_error(
                "self-signed mode cannot also load a certificate or private key",
            ));
        }
        let generated = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ])
        .map_err(|error| tls_error(format!("failed to generate certificate: {error}")))?;
        let certificate = CertificateDer::from(generated.cert);
        let private_key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der())
                .into();
        (vec![certificate], private_key, true)
    } else {
        let cert_path = options.cert_path.as_deref().ok_or_else(|| {
            tls_error("configured certificate mode requires both certificate and private key")
        })?;
        let key_path = options.key_path.as_deref().ok_or_else(|| {
            tls_error("configured certificate mode requires both certificate and private key")
        })?;
        (
            load_certificates(cert_path, "server certificate")?,
            load_private_key(key_path)?,
            false,
        )
    };

    let metadata = CertificateMetadata {
        sha256: fingerprint(&certificates[0]),
        generated,
    };
    let mut rustls_config = rustls::ServerConfig::builder_with_provider(ring_provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| tls_error(format!("invalid TLS protocol versions: {error}")))?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| tls_error(format!("invalid server identity: {error}")))?;
    rustls_config.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    rustls_config.send_tls13_tickets = 0;
    rustls_config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    let crypto = QuicServerConfig::try_from(rustls_config)
        .map_err(|error| tls_error(format!("invalid QUIC server TLS config: {error}")))?;
    Ok((quinn::ServerConfig::with_crypto(Arc::new(crypto)), metadata))
}

/// Builds a Quinn client configuration from an explicit trust mode.
pub fn client_config(options: &QuicClientOptions) -> Result<quinn::ClientConfig> {
    if options.insecure == options.ca_path.is_some() {
        return Err(tls_error(
            "client requires exactly one of insecure mode or a CA PEM",
        ));
    }

    let mut rustls_config = if options.insecure {
        rustls::ClientConfig::builder_with_provider(ring_provider())
            .with_safe_default_protocol_versions()
            .map_err(|error| tls_error(format!("invalid TLS protocol versions: {error}")))?
            .dangerous()
            .with_custom_certificate_verifier(InsecureBenchmarkVerifier::new())
            .with_no_client_auth()
    } else {
        let ca_path = options
            .ca_path
            .as_deref()
            .ok_or_else(|| tls_error("missing CA PEM"))?;
        let mut roots = rustls::RootCertStore::empty();
        for certificate in load_certificates(ca_path, "CA certificate")? {
            roots.add(certificate).map_err(|error| {
                tls_error(format!(
                    "invalid CA certificate in {}: {error}",
                    ca_path.display()
                ))
            })?;
        }
        rustls::ClientConfig::builder_with_provider(ring_provider())
            .with_safe_default_protocol_versions()
            .map_err(|error| tls_error(format!("invalid TLS protocol versions: {error}")))?
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    rustls_config.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    rustls_config.resumption = rustls::client::Resumption::disabled();
    let crypto = QuicClientConfig::try_from(rustls_config)
        .map_err(|error| tls_error(format!("invalid QUIC client TLS config: {error}")))?;
    Ok(quinn::ClientConfig::new(Arc::new(crypto)))
}

/// Certificate verifier used only after explicit benchmark-only opt-in.
#[derive(Debug)]
pub struct InsecureBenchmarkVerifier(Arc<rustls::crypto::CryptoProvider>);

impl InsecureBenchmarkVerifier {
    fn new() -> Arc<Self> {
        Arc::new(Self(ring_provider()))
    }
}

impl ServerCertVerifier for InsecureBenchmarkVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
