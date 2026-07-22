//! Crate-wide error type. All fallible operations in `protocol/` and above
//! return `Result<T>` from this module — sockets, timeouts, and malformed
//! wire data all funnel through here.

use thiserror::Error;

/// Stable user-facing outcome when direct-only WebRTC cannot establish an ICE
/// path. Keep this shared by CLI and TUI so automation and humans see the same
/// explicit no-TURN policy.
pub const WEBRTC_DIRECT_WARNING: &str = "warning: WebRTC direct connection failed; netsu does not use TURN relay, so no throughput test was run";

/// Bounded setup phases shared by optional connection-oriented transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPhase {
    Resolve,
    Bind,
    Tls,
    QuicHandshake,
    SignalingConnect,
    SignalingRoom,
    OfferAnswer,
    IceGathering,
    IceConnected,
    PeerConnected,
    ChannelsOpen,
}

impl std::fmt::Display for SetupPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SetupPhase::Resolve => "resolve",
            SetupPhase::Bind => "bind",
            SetupPhase::Tls => "tls",
            SetupPhase::QuicHandshake => "quic handshake",
            SetupPhase::SignalingConnect => "signaling connect",
            SetupPhase::SignalingRoom => "signaling room",
            SetupPhase::OfferAnswer => "offer/answer",
            SetupPhase::IceGathering => "ICE gathering",
            SetupPhase::IceConnected => "ICE connected",
            SetupPhase::PeerConnected => "peer connected",
            SetupPhase::ChannelsOpen => "channels open",
        })
    }
}

/// Public, non-sensitive reasons for WebRTC setup failure.
///
/// Dependency errors, SDP, ICE candidates, addresses, and listener secrets are
/// deliberately not accepted by this type. Log-free transport code maps its
/// internal error to one of these stable reasons at the boundary.
#[cfg(feature = "webrtc")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRtcSetupFailure {
    SignalingUnavailable,
    SignalingTimedOut,
    RoomUnavailable,
    InvalidRemoteDescription,
    IceGatheringTimedOut,
    DirectPathUnavailable,
    ChannelsTimedOut,
    TransportClosed,
}

#[cfg(feature = "webrtc")]
impl std::fmt::Display for WebRtcSetupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SignalingUnavailable => "signaling service is unavailable",
            Self::SignalingTimedOut => "signaling operation timed out",
            Self::RoomUnavailable => "signaling room is unavailable",
            Self::InvalidRemoteDescription => "remote description was rejected",
            Self::IceGatheringTimedOut => "ICE gathering timed out",
            Self::DirectPathUnavailable => "direct path is unavailable",
            Self::ChannelsTimedOut => "data channels did not open in time",
            Self::TransportClosed => "peer connection closed during setup",
        })
    }
}

/// Creates a WebRTC setup error whose detail cannot contain peer metadata.
#[cfg(feature = "webrtc")]
pub fn webrtc_setup_error(phase: SetupPhase, failure: WebRtcSetupFailure) -> NetsuError {
    NetsuError::Setup {
        transport: "webrtc",
        phase,
        detail: failure.to_string(),
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

/// Whether an error represents the intentional direct-only WebRTC stop case.
pub fn is_webrtc_direct_path_unavailable(error: &NetsuError) -> bool {
    matches!(
        error,
        NetsuError::Setup {
            transport: "webrtc",
            detail,
            ..
        } if detail == "direct path is unavailable"
    )
}

pub type Result<T> = std::result::Result<T, NetsuError>;
