use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time;
use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_candidate_pair::RTCIceCandidatePair;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

use crate::error::{NetsuError, Result, SetupPhase, WebRtcSetupFailure, webrtc_setup_error};

use super::channel::WebRtcChannel;
use super::config::WebRtcOptions;
use super::metrics::{SelectedCandidatePair, WebRtcSetupMetrics};
use super::pipe::{DataChannelSink, WebRtcInbound, WebRtcPipe};
use super::signaling::{
    ClientSignalMessage, DescriptionType, SIGNAL_PROTOCOL_VERSION, ServerSignalMessage,
    SignalingSession,
};

pub const WEBRTC_SUBPROTOCOL: &str = "netsu/iperf3-webrtc/1";
pub const CONTROL_CHANNEL_LABEL: &str = "netsu-control";
pub const MAX_PAYLOAD_CHANNELS: usize = 128;
pub const ICE_GATHER_TIMEOUT: Duration = Duration::from_secs(15);
pub const PEER_CONNECTED_TIMEOUT: Duration = Duration::from_secs(20);
pub const CHANNEL_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
pub const PEER_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

pub fn data_channel_label(index: usize) -> String {
    format!("netsu-data/{index}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMetadata {
    pub label: String,
    pub protocol: String,
    pub ordered: bool,
    pub max_packet_lifetime: Option<u16>,
    pub max_retransmits: Option<u16>,
    pub negotiated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    Control,
    Payload(usize),
}

/// Validates the set of in-band channels announced by the remote peer.
pub struct ChannelManifest {
    parallel: usize,
    accepted: HashSet<ChannelKind>,
}

impl ChannelManifest {
    pub fn new(parallel: usize) -> Result<Self> {
        if parallel > MAX_PAYLOAD_CHANNELS {
            return Err(NetsuError::Protocol(
                "WebRTC payload channel count exceeds 128".into(),
            ));
        }
        Ok(Self {
            parallel,
            accepted: HashSet::with_capacity(parallel.saturating_add(1)),
        })
    }

    pub fn accept(&mut self, metadata: ChannelMetadata) -> Result<ChannelKind> {
        if metadata.protocol != WEBRTC_SUBPROTOCOL {
            return Err(protocol_error("WebRTC DataChannel subprotocol mismatch"));
        }
        if !metadata.ordered
            || metadata.max_packet_lifetime.is_some()
            || metadata.max_retransmits.is_some()
            || metadata.negotiated
        {
            return Err(protocol_error(
                "WebRTC DataChannel must be reliable, ordered, and negotiated in-band",
            ));
        }

        let kind = if metadata.label == CONTROL_CHANNEL_LABEL {
            ChannelKind::Control
        } else if let Some(index) = metadata.label.strip_prefix("netsu-data/") {
            let index = index
                .parse::<usize>()
                .map_err(|_| protocol_error("unknown WebRTC DataChannel label"))?;
            if index >= self.parallel {
                return Err(protocol_error(
                    "unexpected payload WebRTC DataChannel label",
                ));
            }
            ChannelKind::Payload(index)
        } else {
            return Err(protocol_error("unknown WebRTC DataChannel label"));
        };

        if !self.accepted.insert(kind) {
            return Err(protocol_error("duplicate WebRTC DataChannel label"));
        }
        Ok(kind)
    }

    pub fn is_complete(&self) -> bool {
        self.accepted.len() == self.parallel.saturating_add(1)
            && self.accepted.contains(&ChannelKind::Control)
    }
}

fn protocol_error(detail: impl Into<String>) -> NetsuError {
    NetsuError::Protocol(detail.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    Offerer,
    Answerer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalIceEvent {
    Candidate(RTCIceCandidateInit),
    Complete,
}

pub struct NegotiatedWebRtc {
    pub peer: WebRtcPeer,
    pub control: WebRtcPipe,
    pub metrics: WebRtcSetupMetrics,
}

enum OpenChannel {
    Control(WebRtcPipe),
    Payload(WebRtcChannel),
}

struct PendingChannel {
    channel: Option<OpenChannel>,
    opened: oneshot::Receiver<()>,
}

impl PendingChannel {
    async fn wait(mut self) -> Result<OpenChannel> {
        time::timeout(CHANNEL_OPEN_TIMEOUT, &mut self.opened)
            .await
            .map_err(|_| channels_timed_out())?
            .map_err(|_| transport_closed())?;
        self.channel.take().ok_or_else(transport_closed)
    }
}

/// One bounded, data-only PeerConnection. Signaling transports SDP and the
/// [`LocalIceEvent`] stream; this type never logs either.
pub struct WebRtcPeer {
    pc: Arc<RTCPeerConnection>,
    role: PeerRole,
    include_addresses: bool,
    local_ice_rx: mpsc::Receiver<LocalIceEvent>,
    peer_state_rx: watch::Receiver<RTCPeerConnectionState>,
    selected_pair_rx: watch::Receiver<Option<RTCIceCandidatePair>>,
    remote_channel_rx: mpsc::Receiver<Arc<RTCDataChannel>>,
    remote_manifest: ChannelManifest,
    remote_channels: HashMap<ChannelKind, PendingChannel>,
    prepared_control: Option<PendingChannel>,
    remote_description_set: bool,
    buffered_remote_candidates: Vec<RTCIceCandidateInit>,
    remote_candidate_count: usize,
    closed: bool,
}

impl WebRtcPeer {
    pub async fn new(options: &WebRtcOptions, role: PeerRole) -> Result<Self> {
        // reqwest 0.13 enables aws-lc while webrtc-rs' DTLS stack enables ring.
        // rustls intentionally refuses to guess when both are compiled in.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let configuration = RTCConfiguration {
            ice_servers: options
                .stun_urls
                .iter()
                .map(|url| RTCIceServer {
                    urls: vec![url.clone()],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let pc = Arc::new(
            APIBuilder::new()
                .build()
                .new_peer_connection(configuration)
                .await
                .map_err(|_| transport_closed())?,
        );

        let (local_ice_tx, local_ice_rx) = mpsc::channel(17);
        pc.on_ice_candidate(Box::new(move |candidate| {
            let local_ice_tx = local_ice_tx.clone();
            Box::pin(async move {
                let event = match candidate {
                    Some(candidate) => match candidate.to_json() {
                        Ok(candidate) => LocalIceEvent::Candidate(candidate),
                        Err(_) => LocalIceEvent::Complete,
                    },
                    None => LocalIceEvent::Complete,
                };
                let _ = local_ice_tx.send(event).await;
            })
        }));

        let (peer_state_tx, peer_state_rx) = watch::channel(RTCPeerConnectionState::New);
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let peer_state_tx = peer_state_tx.clone();
            Box::pin(async move {
                peer_state_tx.send_replace(state);
            })
        }));

        let (selected_pair_tx, selected_pair_rx) = watch::channel(None);
        pc.sctp()
            .transport()
            .ice_transport()
            .on_selected_candidate_pair_change(Box::new(move |pair| {
                let selected_pair_tx = selected_pair_tx.clone();
                Box::pin(async move {
                    selected_pair_tx.send_replace(Some(pair));
                })
            }));

        let (remote_channel_tx, remote_channel_rx) = mpsc::channel(MAX_PAYLOAD_CHANNELS + 1);
        pc.on_data_channel(Box::new(move |channel| {
            let remote_channel_tx = remote_channel_tx.clone();
            Box::pin(async move {
                let _ = remote_channel_tx.send(channel).await;
            })
        }));

        Ok(Self {
            pc,
            role,
            include_addresses: options.include_addresses,
            local_ice_rx,
            peer_state_rx,
            selected_pair_rx,
            remote_channel_rx,
            remote_manifest: ChannelManifest::new(MAX_PAYLOAD_CHANNELS)?,
            remote_channels: HashMap::new(),
            prepared_control: None,
            remote_description_set: false,
            buffered_remote_candidates: Vec::new(),
            remote_candidate_count: 0,
            closed: false,
        })
    }

    pub async fn prepare_control(&mut self) -> Result<()> {
        if self.role != PeerRole::Offerer || self.prepared_control.is_some() {
            return Err(protocol_error(
                "WebRTC control channel may be prepared once by the offerer",
            ));
        }
        let channel = self.create_data_channel(CONTROL_CHANNEL_LABEL).await?;
        self.prepared_control = Some(attach_channel(channel, ChannelKind::Control).await);
        Ok(())
    }

    pub async fn take_prepared_control(&mut self) -> Result<WebRtcPipe> {
        let pending = self
            .prepared_control
            .take()
            .ok_or_else(|| protocol_error("WebRTC offerer control channel was not prepared"))?;
        match pending.wait().await? {
            OpenChannel::Control(pipe) => Ok(pipe),
            OpenChannel::Payload(_) => Err(protocol_error("invalid WebRTC control channel")),
        }
    }

    pub async fn open_payload(&mut self, index: usize) -> Result<WebRtcChannel> {
        if self.role != PeerRole::Offerer || index >= MAX_PAYLOAD_CHANNELS {
            return Err(protocol_error("invalid WebRTC payload channel index"));
        }
        let label = data_channel_label(index);
        let channel = self.create_data_channel(&label).await?;
        match attach_channel(channel, ChannelKind::Payload(index))
            .await
            .wait()
            .await?
        {
            OpenChannel::Payload(channel) => Ok(channel),
            OpenChannel::Control(_) => Err(protocol_error("invalid WebRTC payload channel")),
        }
    }

    pub async fn accept_control(&mut self) -> Result<WebRtcPipe> {
        match self.accept_remote_channel(ChannelKind::Control).await? {
            OpenChannel::Control(pipe) => Ok(pipe),
            OpenChannel::Payload(_) => Err(protocol_error("invalid WebRTC control channel")),
        }
    }

    pub async fn accept_payload(&mut self, index: usize) -> Result<WebRtcChannel> {
        if index >= MAX_PAYLOAD_CHANNELS {
            return Err(protocol_error("invalid WebRTC payload channel index"));
        }
        match self
            .accept_remote_channel(ChannelKind::Payload(index))
            .await?
        {
            OpenChannel::Payload(channel) => Ok(channel),
            OpenChannel::Control(_) => Err(protocol_error("invalid WebRTC payload channel")),
        }
    }

    pub async fn create_offer(&self) -> Result<String> {
        if self.role != PeerRole::Offerer || self.prepared_control.is_none() {
            return Err(protocol_error(
                "WebRTC offer requires a prepared control channel",
            ));
        }
        let offer = self
            .pc
            .create_offer(None)
            .await
            .map_err(|_| transport_closed())?;
        self.pc
            .set_local_description(offer)
            .await
            .map_err(|_| transport_closed())?;
        self.pc
            .local_description()
            .await
            .map(|description| description.sdp)
            .ok_or_else(transport_closed)
    }

    pub async fn accept_offer(&mut self, sdp: String) -> Result<String> {
        if self.role != PeerRole::Answerer {
            return Err(protocol_error("only the WebRTC answerer accepts offers"));
        }
        let description =
            RTCSessionDescription::offer(sdp).map_err(|_| invalid_remote_description())?;
        self.set_remote_description(description).await?;
        let answer = self
            .pc
            .create_answer(None)
            .await
            .map_err(|_| transport_closed())?;
        self.pc
            .set_local_description(answer)
            .await
            .map_err(|_| transport_closed())?;
        self.pc
            .local_description()
            .await
            .map(|description| description.sdp)
            .ok_or_else(transport_closed)
    }

    pub async fn accept_answer(&mut self, sdp: String) -> Result<()> {
        if self.role != PeerRole::Offerer {
            return Err(protocol_error("only the WebRTC offerer accepts answers"));
        }
        let description =
            RTCSessionDescription::answer(sdp).map_err(|_| invalid_remote_description())?;
        self.set_remote_description(description).await
    }

    pub async fn next_local_ice(&mut self) -> Result<LocalIceEvent> {
        time::timeout(ICE_GATHER_TIMEOUT, self.local_ice_rx.recv())
            .await
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::IceGathering,
                    WebRtcSetupFailure::IceGatheringTimedOut,
                )
            })?
            .ok_or_else(transport_closed)
    }

    pub async fn add_remote_candidate(&mut self, candidate: RTCIceCandidateInit) -> Result<()> {
        if candidate.candidate.len() > super::signaling::MAX_SIGNAL_CANDIDATE_BYTES {
            return Err(protocol_error("WebRTC ICE candidate exceeds 4 KiB"));
        }
        self.remote_candidate_count += 1;
        if self.remote_candidate_count > super::signaling::MAX_SIGNAL_CANDIDATES_PER_PEER {
            return Err(protocol_error("WebRTC ICE candidate limit exceeded"));
        }
        if self.remote_description_set {
            self.pc
                .add_ice_candidate(candidate)
                .await
                .map_err(|_| invalid_remote_description())
        } else {
            self.buffered_remote_candidates.push(candidate);
            Ok(())
        }
    }

    pub async fn wait_for_direct_path(&mut self) -> Result<SelectedCandidatePair> {
        let wait = async {
            loop {
                if let Some(pair) = self.selected_pair_rx.borrow().clone() {
                    return SelectedCandidatePair::from_rtc(pair, self.include_addresses);
                }
                match *self.peer_state_rx.borrow() {
                    RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Closed => return Err(transport_closed()),
                    _ => {}
                }
                tokio::select! {
                    changed = self.peer_state_rx.changed() => {
                        changed.map_err(|_| transport_closed())?;
                    }
                    changed = self.selected_pair_rx.changed() => {
                        changed.map_err(|_| transport_closed())?;
                    }
                }
            }
        };
        time::timeout(PEER_CONNECTED_TIMEOUT, wait)
            .await
            .map_err(|_| {
                webrtc_setup_error(
                    SetupPhase::PeerConnected,
                    WebRtcSetupFailure::DirectPathUnavailable,
                )
            })?
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        time::timeout(PEER_CLOSE_TIMEOUT, self.pc.close())
            .await
            .map_err(|_| transport_closed())?
            .map_err(|_| transport_closed())
    }

    async fn create_data_channel(&self, label: &str) -> Result<Arc<RTCDataChannel>> {
        let init = RTCDataChannelInit {
            ordered: Some(true),
            max_packet_life_time: None,
            max_retransmits: None,
            protocol: Some(WEBRTC_SUBPROTOCOL.to_owned()),
            negotiated: None,
        };
        self.pc
            .create_data_channel(label, Some(init))
            .await
            .map_err(|_| transport_closed())
    }

    async fn set_remote_description(&mut self, description: RTCSessionDescription) -> Result<()> {
        self.pc
            .set_remote_description(description)
            .await
            .map_err(|_| invalid_remote_description())?;
        self.remote_description_set = true;
        for candidate in std::mem::take(&mut self.buffered_remote_candidates) {
            self.pc
                .add_ice_candidate(candidate)
                .await
                .map_err(|_| invalid_remote_description())?;
        }
        Ok(())
    }

    async fn accept_remote_channel(&mut self, expected: ChannelKind) -> Result<OpenChannel> {
        loop {
            if let Some(pending) = self.remote_channels.remove(&expected) {
                return pending.wait().await;
            }
            let channel = time::timeout(CHANNEL_OPEN_TIMEOUT, self.remote_channel_rx.recv())
                .await
                .map_err(|_| channels_timed_out())?
                .ok_or_else(transport_closed)?;
            let kind = self.remote_manifest.accept(channel_metadata(&channel))?;
            let pending = attach_channel(channel, kind).await;
            if self.remote_channels.insert(kind, pending).is_some() {
                return Err(protocol_error("duplicate WebRTC DataChannel label"));
            }
        }
    }
}

/// Offerer/client half of the signaling-v1 negotiation. SDP and candidate
/// values stay typed and are never copied into public errors.
pub async fn negotiate_offerer(
    options: &WebRtcOptions,
    signaling: &mut SignalingSession,
) -> Result<NegotiatedWebRtc> {
    let mut peer = WebRtcPeer::new(options, PeerRole::Offerer).await?;
    let result = negotiate_offerer_inner(&mut peer, signaling).await;
    finish_signaling(signaling).await;
    match result {
        Ok((control, metrics)) => Ok(NegotiatedWebRtc {
            peer,
            control,
            metrics,
        }),
        Err(error) => {
            let _ = peer.close().await;
            Err(error)
        }
    }
}

/// Answerer/server half of the signaling-v1 negotiation.
pub async fn negotiate_answerer(
    options: &WebRtcOptions,
    signaling: &mut SignalingSession,
) -> Result<NegotiatedWebRtc> {
    let mut peer = WebRtcPeer::new(options, PeerRole::Answerer).await?;
    let result = negotiate_answerer_inner(&mut peer, signaling).await;
    finish_signaling(signaling).await;
    match result {
        Ok((control, metrics)) => Ok(NegotiatedWebRtc {
            peer,
            control,
            metrics,
        }),
        Err(error) => {
            let _ = peer.close().await;
            Err(error)
        }
    }
}

async fn negotiate_offerer_inner(
    peer: &mut WebRtcPeer,
    signaling: &mut SignalingSession,
) -> Result<(WebRtcPipe, WebRtcSetupMetrics)> {
    expect_peer_ready(signaling, false).await?;
    let started = Instant::now();
    peer.prepare_control().await?;
    let offer = peer.create_offer().await?;
    signaling
        .send(&ClientSignalMessage::description(
            DescriptionType::Offer,
            offer,
        ))
        .await?;
    send_local_ice(peer, signaling).await?;

    let mut answer_received = false;
    loop {
        match signaling.next().await? {
            ServerSignalMessage::Description {
                sdp_type: DescriptionType::Answer,
                sdp,
                ..
            } if !answer_received => {
                peer.accept_answer(sdp).await?;
                answer_received = true;
            }
            ServerSignalMessage::Candidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
                ..
            } => {
                peer.add_remote_candidate(RTCIceCandidateInit {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                    username_fragment,
                })
                .await?;
            }
            ServerSignalMessage::EndOfCandidates { .. } if answer_received => break,
            ServerSignalMessage::PeerLeft { .. } => return Err(transport_closed()),
            ServerSignalMessage::Error { .. } => return Err(transport_closed()),
            _ => return Err(protocol_error("unexpected WebRTC signaling message")),
        }
    }
    let offer_answer_ms = elapsed_ms(started);
    complete_connection(peer, offer_answer_ms, started).await
}

async fn negotiate_answerer_inner(
    peer: &mut WebRtcPeer,
    signaling: &mut SignalingSession,
) -> Result<(WebRtcPipe, WebRtcSetupMetrics)> {
    expect_peer_ready(signaling, true).await?;
    let started = Instant::now();
    let mut offer_received = false;
    loop {
        match signaling.next().await? {
            ServerSignalMessage::Description {
                sdp_type: DescriptionType::Offer,
                sdp,
                ..
            } if !offer_received => {
                let answer = peer.accept_offer(sdp).await?;
                offer_received = true;
                signaling
                    .send(&ClientSignalMessage::description(
                        DescriptionType::Answer,
                        answer,
                    ))
                    .await?;
                send_local_ice(peer, signaling).await?;
            }
            ServerSignalMessage::Candidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
                ..
            } => {
                peer.add_remote_candidate(RTCIceCandidateInit {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                    username_fragment,
                })
                .await?;
            }
            ServerSignalMessage::EndOfCandidates { .. } if offer_received => break,
            ServerSignalMessage::PeerLeft { .. } => return Err(transport_closed()),
            ServerSignalMessage::Error { .. } => return Err(transport_closed()),
            _ => return Err(protocol_error("unexpected WebRTC signaling message")),
        }
    }
    let offer_answer_ms = elapsed_ms(started);
    complete_connection(peer, offer_answer_ms, started).await
}

async fn expect_peer_ready(signaling: &mut SignalingSession, listener_wait: bool) -> Result<()> {
    let message = if listener_wait {
        signaling.wait_for_peer().await?
    } else {
        signaling.next().await?
    };
    match message {
        ServerSignalMessage::PeerReady { .. } => Ok(()),
        ServerSignalMessage::PeerLeft { .. } | ServerSignalMessage::Error { .. } => {
            Err(transport_closed())
        }
        _ => Err(protocol_error(
            "signaling peer was not ready for WebRTC negotiation",
        )),
    }
}

async fn send_local_ice(peer: &mut WebRtcPeer, signaling: &mut SignalingSession) -> Result<()> {
    loop {
        match peer.next_local_ice().await? {
            LocalIceEvent::Candidate(candidate) => {
                signaling
                    .send(&ClientSignalMessage::Candidate {
                        v: SIGNAL_PROTOCOL_VERSION,
                        candidate: candidate.candidate,
                        sdp_mid: candidate.sdp_mid,
                        sdp_mline_index: candidate.sdp_mline_index,
                        username_fragment: candidate.username_fragment,
                    })
                    .await?;
            }
            LocalIceEvent::Complete => {
                return signaling
                    .send(&ClientSignalMessage::EndOfCandidates {
                        v: SIGNAL_PROTOCOL_VERSION,
                    })
                    .await;
            }
        }
    }
}

async fn complete_connection(
    peer: &mut WebRtcPeer,
    offer_answer_ms: f64,
    started: Instant,
) -> Result<(WebRtcPipe, WebRtcSetupMetrics)> {
    let selected_pair = peer.wait_for_direct_path().await?;
    let ice_connected_ms = elapsed_ms(started);
    let control = match peer.role {
        PeerRole::Offerer => peer.take_prepared_control().await?,
        PeerRole::Answerer => peer.accept_control().await?,
    };
    let channels_open_ms = elapsed_ms(started);
    Ok((
        control,
        WebRtcSetupMetrics {
            offer_answer_ms,
            ice_connected_ms,
            channels_open_ms,
            selected_pair,
        },
    ))
}

async fn finish_signaling(signaling: &mut SignalingSession) {
    let _ = signaling.leave().await;
    let _ = signaling.close().await;
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn channel_metadata(channel: &RTCDataChannel) -> ChannelMetadata {
    ChannelMetadata {
        label: channel.label().to_owned(),
        protocol: channel.protocol().to_owned(),
        ordered: channel.ordered(),
        max_packet_lifetime: channel.max_packet_lifetime(),
        max_retransmits: channel.max_retransmits(),
        negotiated: channel.negotiated(),
    }
}

async fn attach_channel(channel: Arc<RTCDataChannel>, kind: ChannelKind) -> PendingChannel {
    let sink = Arc::new(RtcDataChannelSink::new(Arc::clone(&channel)).await);
    let (opened_channel, inbound) = match kind {
        ChannelKind::Control => {
            let (pipe, inbound) = WebRtcPipe::new(sink.clone());
            (OpenChannel::Control(pipe), inbound)
        }
        ChannelKind::Payload(_) => {
            let (channel, inbound) = WebRtcChannel::new(sink.clone());
            (OpenChannel::Payload(channel), inbound)
        }
    };

    attach_inbound_callbacks(&channel, inbound);
    let (opened_tx, opened_rx) = oneshot::channel();
    channel.on_open(Box::new(move || {
        Box::pin(async move {
            sink.mark_open().await;
            let _ = opened_tx.send(());
        })
    }));
    PendingChannel {
        channel: Some(opened_channel),
        opened: opened_rx,
    }
}

fn attach_inbound_callbacks(channel: &RTCDataChannel, inbound: WebRtcInbound) {
    let message_inbound = inbound.clone();
    channel.on_message(Box::new(move |message| {
        let inbound = message_inbound.clone();
        Box::pin(async move {
            if message.is_string {
                let _ = inbound.feed_text("").await;
            } else {
                let _ = inbound.feed_binary(&message.data).await;
            }
        })
    }));
    let close_inbound = inbound.clone();
    channel.on_close(Box::new(move || {
        let inbound = close_inbound.clone();
        Box::pin(async move {
            inbound.close().await;
        })
    }));
    channel.on_error(Box::new(move |_| {
        let inbound = inbound.clone();
        Box::pin(async move {
            inbound.fail().await;
        })
    }));
}

struct RtcDataChannelSink {
    channel: Arc<RTCDataChannel>,
    buffered_low: Arc<Notify>,
    open_buffer_floor: AtomicUsize,
}

impl RtcDataChannelSink {
    async fn new(channel: Arc<RTCDataChannel>) -> Self {
        let buffered_low = Arc::new(Notify::new());
        let callback_notify = Arc::clone(&buffered_low);
        channel
            .on_buffered_amount_low(Box::new(move || {
                callback_notify.notify_waiters();
                Box::pin(async {})
            }))
            .await;
        Self {
            channel,
            buffered_low,
            open_buffer_floor: AtomicUsize::new(0),
        }
    }

    async fn mark_open(&self) {
        // The pinned webrtc-rs line includes its DCEP OPEN frame in the SCTP
        // stream's buffered amount even though it is not application payload.
        // Record that immutable floor once so netsu drains only bytes it sent.
        self.open_buffer_floor
            .store(self.channel.buffered_amount().await, Ordering::Release);
    }

    fn floor(&self) -> usize {
        self.open_buffer_floor.load(Ordering::Acquire)
    }
}

#[async_trait]
impl DataChannelSink for RtcDataChannelSink {
    async fn send_binary(&self, data: &[u8]) -> Result<()> {
        let data_len = data.len();
        let sent = time::timeout(
            super::pipe::DATA_CHANNEL_DRAIN_TIMEOUT,
            self.channel.send(&data.to_vec().into()),
        )
        .await
        .map_err(|_| protocol_error("WebRTC DataChannel send timed out"))?
        .map_err(|_| protocol_error("WebRTC DataChannel send failed"))?;
        if sent != data_len {
            return Err(protocol_error("WebRTC DataChannel short send"));
        }
        Ok(())
    }

    async fn buffered_amount(&self) -> usize {
        self.channel
            .buffered_amount()
            .await
            .saturating_sub(self.floor())
    }

    async fn set_buffered_amount_low_threshold(&self, bytes: usize) {
        self.channel
            .set_buffered_amount_low_threshold(self.floor().saturating_add(bytes))
            .await;
    }

    async fn wait_buffered_amount_at_most(&self, maximum: usize) {
        loop {
            let notified = self.buffered_low.notified();
            if self.channel.buffered_amount().await <= self.floor().saturating_add(maximum) {
                return;
            }
            // webrtc-rs can release the final buffered bytes in the narrow
            // window before a newly installed zero-threshold callback becomes
            // observable. The event remains the fast path; this coarse timer
            // is only a lost-wakeup guard and is never a zero-delay spin.
            tokio::select! {
                _ = notified => {}
                _ = time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    fn defers_close_to_peer(&self) -> bool {
        true
    }
}

fn invalid_remote_description() -> NetsuError {
    webrtc_setup_error(
        SetupPhase::OfferAnswer,
        WebRtcSetupFailure::InvalidRemoteDescription,
    )
}

fn channels_timed_out() -> NetsuError {
    webrtc_setup_error(
        SetupPhase::ChannelsOpen,
        WebRtcSetupFailure::ChannelsTimedOut,
    )
}

fn transport_closed() -> NetsuError {
    webrtc_setup_error(
        SetupPhase::PeerConnected,
        WebRtcSetupFailure::TransportClosed,
    )
}
