use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::time;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::error::{NetsuError, Result, SetupPhase, WebRtcSetupFailure, webrtc_setup_error};

use super::config::WebRtcOptions;

pub const SIGNAL_PROTOCOL_VERSION: u8 = 1;
pub const MAX_SIGNAL_FRAME_BYTES: usize = 65_536;
pub const MAX_SIGNAL_SDP_BYTES: usize = 60 * 1_024;
pub const MAX_SIGNAL_CANDIDATE_BYTES: usize = 4_096;
pub const MAX_SIGNAL_CANDIDATES_PER_PEER: usize = 16;
pub const MAX_SIGNAL_FORWARDED_BYTES: usize = 1_048_576;
pub const SIGNAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SIGNAL_BIND_TIMEOUT: Duration = Duration::from_secs(10);
pub const SIGNAL_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(15);
pub const SIGNAL_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

type SignalWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SignalWriter = SplitSink<SignalWebSocket, Message>;
type SignalReader = SplitStream<SignalWebSocket>;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientSignalMessage {
    Bind {
        v: u8,
        role: SignalRole,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_optional_secret",
            deserialize_with = "deserialize_optional_secret"
        )]
        secret: Option<SecretString>,
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
            secret: Some(SecretString::from(secret.into())),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalRoom {
    pub code: String,
    pub expires_at: String,
}

pub struct ListenerRegistration {
    pub room: SignalRoom,
    pub session: SignalingSession,
}

#[derive(Serialize)]
struct CreateRoomRequest {
    v: u8,
    ttl_seconds: u32,
}

#[derive(Deserialize)]
struct CreateRoomResponse {
    v: u8,
    code: String,
    #[serde(deserialize_with = "deserialize_secret")]
    listener_secret: SecretString,
    expires_at: String,
}

/// Bounded client for RendezKey's signaling-v1 HTTP and WebSocket surfaces.
pub struct SignalingClient {
    options: WebRtcOptions,
    api_token: Option<SecretString>,
    http: reqwest::Client,
}

impl SignalingClient {
    pub fn new(options: WebRtcOptions, api_token: Option<SecretString>) -> Self {
        Self {
            options,
            api_token,
            http: reqwest::Client::new(),
        }
    }

    pub async fn create_listener(&self, ttl_seconds: u32) -> Result<ListenerRegistration> {
        if !(60..=3_600).contains(&ttl_seconds) {
            return Err(protocol_error(
                "signaling room TTL must be 60..3600 seconds",
            ));
        }
        let rooms_url = self.options.rooms_url()?;
        let request_body = serde_json::to_vec(&CreateRoomRequest {
            v: SIGNAL_PROTOCOL_VERSION,
            ttl_seconds,
        })?;
        let mut request = self
            .http
            .post(rooms_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_body);
        if let Some(token) = &self.api_token {
            request = request.bearer_auth(token.expose_secret());
        }
        let response = time::timeout(SIGNAL_CONNECT_TIMEOUT, request.send())
            .await
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingRoom,
                    WebRtcSetupFailure::SignalingTimedOut,
                )
            })?
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingRoom,
                    WebRtcSetupFailure::SignalingUnavailable,
                )
            })?;
        if response.status() != reqwest::StatusCode::CREATED {
            return Err(webrtc_setup_error(
                SetupPhase::SignalingRoom,
                WebRtcSetupFailure::RoomUnavailable,
            ));
        }
        let response_body = time::timeout(SIGNAL_BIND_TIMEOUT, response.bytes())
            .await
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingRoom,
                    WebRtcSetupFailure::SignalingTimedOut,
                )
            })?
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingRoom,
                    WebRtcSetupFailure::RoomUnavailable,
                )
            })?;
        if response_body.len() > MAX_SIGNAL_FRAME_BYTES {
            return Err(protocol_error("signaling room response exceeds 64 KiB"));
        }
        let response: CreateRoomResponse =
            serde_json::from_slice(&response_body).map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingRoom,
                    WebRtcSetupFailure::RoomUnavailable,
                )
            })?;
        if response.v != SIGNAL_PROTOCOL_VERSION {
            return Err(protocol_error("unsupported signaling protocol version"));
        }
        let code = normalize_room_code(&response.code)?;
        let bind = ClientSignalMessage::Bind {
            v: SIGNAL_PROTOCOL_VERSION,
            role: SignalRole::Listener,
            secret: Some(response.listener_secret),
        };
        let session = self
            .connect_and_bind(&code, SignalRole::Listener, bind)
            .await?;

        Ok(ListenerRegistration {
            room: SignalRoom {
                code,
                expires_at: response.expires_at,
            },
            session,
        })
    }

    pub async fn join(&self, code: &str) -> Result<SignalingSession> {
        let code = normalize_room_code(code)?;
        self.connect_and_bind(
            &code,
            SignalRole::Joiner,
            ClientSignalMessage::bind_joiner(),
        )
        .await
    }

    async fn connect_and_bind(
        &self,
        code: &str,
        expected_role: SignalRole,
        bind: ClientSignalMessage,
    ) -> Result<SignalingSession> {
        let websocket_url = self.options.room_websocket_url(code)?;
        let (websocket, _) = time::timeout(
            SIGNAL_CONNECT_TIMEOUT,
            connect_async(websocket_url.as_str()),
        )
        .await
        .map_err(|_| {
            webrtc_setup_error(
                SetupPhase::SignalingConnect,
                WebRtcSetupFailure::SignalingTimedOut,
            )
        })?
        .map_err(|_| {
            webrtc_setup_error(
                SetupPhase::SignalingConnect,
                WebRtcSetupFailure::SignalingUnavailable,
            )
        })?;
        let (writer, reader) = websocket.split();
        let mut session = SignalingSession {
            writer,
            reader,
            sent_candidates: 0,
            sent_bytes: 0,
            closed: false,
        };
        session
            .send_with_timeout(&bind, SIGNAL_BIND_TIMEOUT)
            .await?;
        match session
            .next_with_timeout(SIGNAL_BIND_TIMEOUT, SetupPhase::SignalingRoom)
            .await?
        {
            ServerSignalMessage::Bound { role, .. } if role == expected_role => Ok(session),
            ServerSignalMessage::Error { .. } => Err(webrtc_setup_error(
                SetupPhase::SignalingRoom,
                WebRtcSetupFailure::RoomUnavailable,
            )),
            _ => Err(protocol_error(
                "signaling peer did not acknowledge the expected role",
            )),
        }
    }
}

/// One bound signaling socket. SDP/ICE payloads are never included in errors.
pub struct SignalingSession {
    writer: SignalWriter,
    reader: SignalReader,
    sent_candidates: usize,
    sent_bytes: usize,
    closed: bool,
}

impl SignalingSession {
    pub async fn send(&mut self, message: &ClientSignalMessage) -> Result<()> {
        self.send_with_timeout(message, SIGNAL_EXCHANGE_TIMEOUT)
            .await
    }

    pub async fn next(&mut self) -> Result<ServerSignalMessage> {
        self.next_with_timeout(SIGNAL_EXCHANGE_TIMEOUT, SetupPhase::OfferAnswer)
            .await
    }

    pub async fn leave(&mut self) -> Result<()> {
        if !self.closed {
            self.send_with_timeout(
                &ClientSignalMessage::Leave {
                    v: SIGNAL_PROTOCOL_VERSION,
                },
                SIGNAL_CLOSE_TIMEOUT,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let _ = time::timeout(SIGNAL_CLOSE_TIMEOUT, self.writer.close()).await;
        Ok(())
    }

    async fn send_with_timeout(
        &mut self,
        message: &ClientSignalMessage,
        deadline: Duration,
    ) -> Result<()> {
        if self.closed {
            return Err(webrtc_setup_error(
                SetupPhase::SignalingConnect,
                WebRtcSetupFailure::TransportClosed,
            ));
        }
        if matches!(message, ClientSignalMessage::Candidate { .. }) {
            self.sent_candidates += 1;
            if self.sent_candidates > MAX_SIGNAL_CANDIDATES_PER_PEER {
                return Err(protocol_error("signaling candidate limit exceeded"));
            }
        }
        let frame = encode_client_message(message)?;
        self.sent_bytes = self.sent_bytes.saturating_add(frame.len());
        if self.sent_bytes > MAX_SIGNAL_FORWARDED_BYTES {
            return Err(protocol_error("signaling room byte limit exceeded"));
        }
        time::timeout(deadline, self.writer.send(Message::Text(frame)))
            .await
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingConnect,
                    WebRtcSetupFailure::SignalingTimedOut,
                )
            })?
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::SignalingConnect,
                    WebRtcSetupFailure::TransportClosed,
                )
            })
    }

    async fn next_with_timeout(
        &mut self,
        deadline: Duration,
        phase: SetupPhase,
    ) -> Result<ServerSignalMessage> {
        time::timeout(deadline, async {
            loop {
                match self.reader.next().await {
                    Some(Ok(Message::Text(frame))) => return decode_server_message(&frame),
                    Some(Ok(Message::Ping(payload))) => {
                        self.writer
                            .send(Message::Pong(payload))
                            .await
                            .map_err(|_| {
                                webrtc_setup_error(phase, WebRtcSetupFailure::TransportClosed)
                            })?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) | Some(Ok(Message::Frame(_))) => {
                        return Err(protocol_error("signaling server sent a non-text frame"));
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        self.closed = true;
                        return Err(webrtc_setup_error(
                            phase,
                            WebRtcSetupFailure::TransportClosed,
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|_| webrtc_setup_error(phase, WebRtcSetupFailure::SignalingTimedOut))?
    }
}

impl Drop for SignalingSession {
    fn drop(&mut self) {
        // Split WebSocket halves close their underlying stream on drop. Async
        // callers should use `leave` + `close`; this is the panic/cancellation
        // fallback and deliberately performs no blocking work.
        self.closed = true;
    }
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

fn serialize_optional_secret<S>(
    secret: &Option<SecretString>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match secret {
        Some(secret) => serializer.serialize_some(secret.expose_secret()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_secret<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|secret| secret.map(SecretString::from))
}

fn deserialize_secret<'de, D>(deserializer: D) -> std::result::Result<SecretString, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(SecretString::from)
}

fn normalize_room_code(input: &str) -> Result<String> {
    const ALPHABET: &str = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let normalized = input
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '-')
        .flat_map(char::to_uppercase)
        .collect::<String>();
    if normalized.len() != 8
        || !normalized
            .chars()
            .all(|character| ALPHABET.contains(character))
    {
        return Err(protocol_error("invalid signaling room code"));
    }
    Ok(format!("{}-{}", &normalized[..4], &normalized[4..]))
}
