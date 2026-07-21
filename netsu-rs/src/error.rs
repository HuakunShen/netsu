//! Crate-wide error type. All fallible operations in `protocol/` and above
//! return `Result<T>` from this module — sockets, timeouts, and malformed
//! wire data all funnel through here.

use thiserror::Error;

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
}

pub type Result<T> = std::result::Result<T, NetsuError>;
