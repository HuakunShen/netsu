use serde::{Deserialize, Serialize};

use crate::error::{NetsuError, Result};

pub const SIGNAL_PROTOCOL_VERSION: u8 = 1;
pub const MAX_SIGNAL_FRAME_BYTES: usize = 65_536;
pub const MAX_SIGNAL_SDP_BYTES: usize = 60 * 1_024;
pub const MAX_SIGNAL_CANDIDATE_BYTES: usize = 4_096;
pub const MAX_SIGNAL_CANDIDATES_PER_PEER: usize = 16;
pub const MAX_SIGNAL_FORWARDED_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalRole {
    Listener,
    Joiner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DescriptionType {
    Offer,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientSignalMessage {
    Bind {
        v: u8,
        role: SignalRole,
        #[serde(skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
    },
    Description {
        v: u8,
        sdp_type: DescriptionType,
        sdp: String,
    },
    Candidate {
        v: u8,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    },
    EndOfCandidates {
        v: u8,
    },
    Leave {
        v: u8,
    },
}

impl ClientSignalMessage {
    pub fn bind_listener(secret: impl Into<String>) -> Self {
        Self::Bind {
            v: SIGNAL_PROTOCOL_VERSION,
            role: SignalRole::Listener,
            secret: Some(secret.into()),
        }
    }

    pub fn bind_joiner() -> Self {
        Self::Bind {
            v: SIGNAL_PROTOCOL_VERSION,
            role: SignalRole::Joiner,
            secret: None,
        }
    }

    pub fn description(sdp_type: DescriptionType, sdp: impl Into<String>) -> Self {
        Self::Description {
            v: SIGNAL_PROTOCOL_VERSION,
            sdp_type,
            sdp: sdp.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerSignalMessage {
    Bound {
        v: u8,
        role: SignalRole,
        expires_in_seconds: u32,
    },
    PeerReady {
        v: u8,
    },
    Description {
        v: u8,
        sdp_type: DescriptionType,
        sdp: String,
    },
    Candidate {
        v: u8,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    },
    EndOfCandidates {
        v: u8,
    },
    PeerLeft {
        v: u8,
    },
    Error {
        v: u8,
        code: SignalErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalErrorCode {
    InvalidMessage,
    RoomNotFound,
    RoomExpired,
    RoomFull,
    UnauthorizedListener,
    UnexpectedMessage,
    ResourceLimit,
    InternalError,
}

pub fn encode_client_message(message: &ClientSignalMessage) -> Result<String> {
    validate_client_message(message)?;
    let frame = serde_json::to_string(message)?;
    if frame.len() > MAX_SIGNAL_FRAME_BYTES {
        return Err(protocol_error("signaling frame exceeds 64 KiB"));
    }
    Ok(frame)
}

pub fn decode_server_message(frame: &str) -> Result<ServerSignalMessage> {
    if frame.len() > MAX_SIGNAL_FRAME_BYTES {
        return Err(protocol_error("signaling frame exceeds 64 KiB"));
    }
    let message: ServerSignalMessage =
        serde_json::from_str(frame).map_err(|_| protocol_error("invalid signaling message"))?;
    validate_server_message(&message)?;
    Ok(message)
}

fn validate_client_message(message: &ClientSignalMessage) -> Result<()> {
    let version = match message {
        ClientSignalMessage::Bind { v, role, secret } => {
            if (*role == SignalRole::Listener) != secret.is_some() {
                return Err(protocol_error("listener bind requires exactly one secret"));
            }
            *v
        }
        ClientSignalMessage::Description { v, sdp, .. } => {
            validate_bounded_text(sdp, MAX_SIGNAL_SDP_BYTES, "SDP")?;
            *v
        }
        ClientSignalMessage::Candidate {
            v,
            candidate,
            sdp_mid,
            username_fragment,
            ..
        } => {
            validate_bounded_text(candidate, MAX_SIGNAL_CANDIDATE_BYTES, "candidate")?;
            validate_optional_short(sdp_mid, "SDP mid")?;
            validate_optional_short(username_fragment, "username fragment")?;
            *v
        }
        ClientSignalMessage::EndOfCandidates { v } | ClientSignalMessage::Leave { v } => *v,
    };
    validate_version(version)
}

fn validate_server_message(message: &ServerSignalMessage) -> Result<()> {
    let version = match message {
        ServerSignalMessage::Bound {
            v,
            expires_in_seconds,
            ..
        } => {
            if *expires_in_seconds > 3_600 {
                return Err(protocol_error("invalid signaling room expiry"));
            }
            *v
        }
        ServerSignalMessage::PeerReady { v }
        | ServerSignalMessage::EndOfCandidates { v }
        | ServerSignalMessage::PeerLeft { v } => *v,
        ServerSignalMessage::Description { v, sdp, .. } => {
            validate_bounded_text(sdp, MAX_SIGNAL_SDP_BYTES, "SDP")?;
            *v
        }
        ServerSignalMessage::Candidate {
            v,
            candidate,
            sdp_mid,
            username_fragment,
            ..
        } => {
            validate_bounded_text(candidate, MAX_SIGNAL_CANDIDATE_BYTES, "candidate")?;
            validate_optional_short(sdp_mid, "SDP mid")?;
            validate_optional_short(username_fragment, "username fragment")?;
            *v
        }
        ServerSignalMessage::Error { v, message, .. } => {
            validate_bounded_text(message, 256, "error message")?;
            *v
        }
    };
    validate_version(version)
}

fn validate_version(version: u8) -> Result<()> {
    if version != SIGNAL_PROTOCOL_VERSION {
        return Err(protocol_error("unsupported signaling protocol version"));
    }
    Ok(())
}

fn validate_bounded_text(value: &str, maximum: usize, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > maximum {
        return Err(protocol_error(format!("invalid {label} length")));
    }
    Ok(())
}

fn validate_optional_short(value: &Option<String>, label: &str) -> Result<()> {
    if value.as_ref().is_some_and(|value| value.len() > 256) {
        return Err(protocol_error(format!("invalid {label} length")));
    }
    Ok(())
}

fn protocol_error(detail: impl Into<String>) -> NetsuError {
    NetsuError::Protocol(detail.into())
}
