//! Quinn endpoint ownership, setup timeouts, and bounded shutdown.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::error::{NetsuError, Result, SetupPhase};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const STREAMS_TIMEOUT: Duration = Duration::from_secs(10);
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

fn setup_error(phase: SetupPhase, detail: impl Into<String>) -> NetsuError {
    NetsuError::Setup {
        transport: "quic",
        phase,
        detail: detail.into(),
    }
}

/// Owns one Quinn endpoint and keeps transport lifecycle out of protocol code.
pub struct QuicEndpoint {
    endpoint: quinn::Endpoint,
}

impl QuicEndpoint {
    /// Bind a server endpoint without accepting any connection yet.
    pub fn bind_server(address: SocketAddr, config: quinn::ServerConfig) -> Result<Self> {
        let endpoint = quinn::Endpoint::server(config, address)
            .map_err(|error| setup_error(SetupPhase::Bind, error.to_string()))?;
        Ok(Self { endpoint })
    }

    /// Bind an ephemeral IPv4 client endpoint and install its explicit TLS mode.
    pub fn bind_client(config: quinn::ClientConfig) -> Result<Self> {
        let address = SocketAddr::from(([0, 0, 0, 0], 0));
        let mut endpoint = quinn::Endpoint::client(address)
            .map_err(|error| setup_error(SetupPhase::Bind, error.to_string()))?;
        endpoint.set_default_client_config(config);
        Ok(Self { endpoint })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint
            .local_addr()
            .map_err(|error| setup_error(SetupPhase::Bind, error.to_string()))
    }

    /// Connect with a hard handshake deadline and return its measured duration.
    pub async fn connect(
        &self,
        address: SocketAddr,
        server_name: &str,
    ) -> Result<(quinn::Connection, Duration)> {
        let connecting = self
            .endpoint
            .connect(address, server_name)
            .map_err(|error| setup_error(SetupPhase::QuicHandshake, error.to_string()))?;
        let started = Instant::now();
        let connection = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| {
                setup_error(
                    SetupPhase::QuicHandshake,
                    format!("timed out after {} seconds", CONNECT_TIMEOUT.as_secs()),
                )
            })?
            .map_err(|error| setup_error(SetupPhase::QuicHandshake, error.to_string()))?;
        Ok((connection, started.elapsed()))
    }

    /// Accept one incoming connection with the same bounded handshake policy.
    pub async fn accept(&self) -> Result<(quinn::Connection, Duration)> {
        let incoming = self.endpoint.accept().await.ok_or_else(|| {
            setup_error(SetupPhase::QuicHandshake, "endpoint closed while accepting")
        })?;
        let started = Instant::now();
        let connection = tokio::time::timeout(CONNECT_TIMEOUT, incoming)
            .await
            .map_err(|_| {
                setup_error(
                    SetupPhase::QuicHandshake,
                    format!("timed out after {} seconds", CONNECT_TIMEOUT.as_secs()),
                )
            })?
            .map_err(|error| setup_error(SetupPhase::QuicHandshake, error.to_string()))?;
        Ok((connection, started.elapsed()))
    }

    /// Stop new work and wait at most two seconds for Quinn to drain.
    pub async fn close(self) {
        self.endpoint.close(0u32.into(), b"netsu endpoint closing");
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.endpoint.wait_idle()).await;
    }
}
