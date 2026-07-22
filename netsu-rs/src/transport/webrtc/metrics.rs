use crate::error::{NetsuError, Result, SetupPhase, WebRtcSetupFailure, webrtc_setup_error};

#[derive(Debug, Clone)]
pub struct WebRtcSetupMetrics {
    pub offer_answer_ms: f64,
    pub ice_connected_ms: f64,
    pub channels_open_ms: f64,
    pub selected_pair: SelectedCandidatePair,
}

/// Stable candidate vocabulary used by JSON diagnostics and direct-path policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Host,
    ServerReflexive,
    PeerReflexive,
    Relay,
    Unknown,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::ServerReflexive => "srflx",
            Self::PeerReflexive => "prflx",
            Self::Relay => "relay",
            Self::Unknown => "unknown",
        }
    }

    fn is_direct(self) -> bool {
        matches!(
            self,
            Self::Host | Self::ServerReflexive | Self::PeerReflexive
        )
    }
}

/// ICE transport protocol, normalized without exposing dependency enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceProtocol {
    Udp,
    Tcp,
    Unknown,
}

impl IceProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCandidate {
    pub kind: CandidateKind,
    pub protocol: IceProtocol,
    /// Present only when the caller explicitly enabled address diagnostics.
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCandidatePair {
    pub path: &'static str,
    pub local: SelectedCandidate,
    pub remote: SelectedCandidate,
}

impl SelectedCandidatePair {
    /// Enforces the benchmark's direct-only boundary before any payload starts.
    pub fn new(
        mut local: SelectedCandidate,
        mut remote: SelectedCandidate,
        include_addresses: bool,
    ) -> Result<Self> {
        if !local.kind.is_direct() || !remote.kind.is_direct() {
            return Err(webrtc_setup_error(
                SetupPhase::IceConnected,
                WebRtcSetupFailure::DirectPathUnavailable,
            ));
        }
        if matches!(local.protocol, IceProtocol::Unknown)
            || matches!(remote.protocol, IceProtocol::Unknown)
        {
            return Err(webrtc_setup_error(
                SetupPhase::IceConnected,
                WebRtcSetupFailure::DirectPathUnavailable,
            ));
        }
        if local.protocol != remote.protocol {
            return Err(NetsuError::Protocol(
                "WebRTC selected candidate pair has mismatched protocols".into(),
            ));
        }
        if !include_addresses {
            local.address = None;
            remote.address = None;
        }
        Ok(Self {
            path: "direct",
            local,
            remote,
        })
    }

    pub(crate) fn from_rtc(
        pair: webrtc::ice_transport::ice_candidate_pair::RTCIceCandidatePair,
        include_addresses: bool,
    ) -> Result<Self> {
        fn convert(
            candidate: webrtc::ice_transport::ice_candidate::RTCIceCandidate,
        ) -> SelectedCandidate {
            let address = if candidate.address.contains(':') {
                format!("[{}]:{}", candidate.address, candidate.port)
            } else {
                format!("{}:{}", candidate.address, candidate.port)
            };
            SelectedCandidate {
                kind: candidate.typ.into(),
                protocol: candidate.protocol.into(),
                address: Some(address),
            }
        }

        Self::new(convert(pair.local), convert(pair.remote), include_addresses)
    }
}

impl From<webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType> for CandidateKind {
    fn from(value: webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType) -> Self {
        use webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;
        match value {
            RTCIceCandidateType::Host => Self::Host,
            RTCIceCandidateType::Srflx => Self::ServerReflexive,
            RTCIceCandidateType::Prflx => Self::PeerReflexive,
            RTCIceCandidateType::Relay => Self::Relay,
            RTCIceCandidateType::Unspecified => Self::Unknown,
        }
    }
}

impl From<webrtc::ice_transport::ice_protocol::RTCIceProtocol> for IceProtocol {
    fn from(value: webrtc::ice_transport::ice_protocol::RTCIceProtocol) -> Self {
        use webrtc::ice_transport::ice_protocol::RTCIceProtocol;
        match value {
            RTCIceProtocol::Udp => Self::Udp,
            RTCIceProtocol::Tcp => Self::Tcp,
            RTCIceProtocol::Unspecified => Self::Unknown,
        }
    }
}
