//! Crate-wide error type. All fallible operations in `protocol/` and above
//! return `Result<T>` from this module — sockets, timeouts, and malformed
//! wire data all funnel through here.

use thiserror::Error;

/// Bounded setup phases shared by optional connection-oriented transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPhase {
    Resolve,
    Bind,
    Tls,
    QuicHandshake,
    ChannelsOpen,
}

impl std::fmt::Display for SetupPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SetupPhase::Resolve => "resolve",
            SetupPhase::Bind => "bind",
            SetupPhase::Tls => "tls",
            SetupPhase::QuicHandshake => "quic handshake",
            SetupPhase::ChannelsOpen => "channels open",
        })
    }
}

#[derive(Debug, Error)]
pub enum NetsuError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("operation timed out")]
    Timeout,

    #[error("pipe closed")]
    PipeClosed,

    #[error("server busy")]
    ServerBusy,

    #[error("server error")]
    ServerError,

    #[error("{transport} setup failed during {phase}: {detail}")]
    Setup {
        transport: &'static str,
        phase: SetupPhase,
        detail: String,
    },
}

pub type Result<T> = std::result::Result<T, NetsuError>;
