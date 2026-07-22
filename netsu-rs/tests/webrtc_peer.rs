#![cfg(feature = "webrtc")]

use netsu::protocol::pipe::BytePipe;
use netsu::transport::webrtc::WebRtcOptions;
use netsu::transport::webrtc::metrics::{
    CandidateKind, IceProtocol, SelectedCandidate, SelectedCandidatePair,
};
use netsu::transport::webrtc::peer::{
    CONTROL_CHANNEL_LABEL, ChannelKind, ChannelManifest, ChannelMetadata, LocalIceEvent, PeerRole,
    WEBRTC_SUBPROTOCOL, WebRtcPeer, data_channel_label,
};

fn reliable(label: &str) -> ChannelMetadata {
    ChannelMetadata {
        label: label.to_owned(),
        protocol: WEBRTC_SUBPROTOCOL.to_owned(),
        ordered: true,
        max_packet_lifetime: None,
        max_retransmits: None,
        negotiated: false,
    }
}

#[test]
fn manifest_accepts_one_control_and_exact_payload_labels() {
    let mut manifest = ChannelManifest::new(2).expect("valid manifest");

    assert_eq!(
        manifest.accept(reliable(CONTROL_CHANNEL_LABEL)).unwrap(),
        ChannelKind::Control
    );
    assert_eq!(
        manifest.accept(reliable(&data_channel_label(1))).unwrap(),
        ChannelKind::Payload(1)
    );
    assert_eq!(
        manifest.accept(reliable(&data_channel_label(0))).unwrap(),
        ChannelKind::Payload(0)
    );
    assert!(manifest.is_complete());
}

#[test]
fn manifest_rejects_duplicate_unknown_and_out_of_range_labels() {
    let mut duplicate = ChannelManifest::new(1).unwrap();
    duplicate.accept(reliable(CONTROL_CHANNEL_LABEL)).unwrap();
    assert!(
        duplicate
            .accept(reliable(CONTROL_CHANNEL_LABEL))
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );

    let mut unknown = ChannelManifest::new(1).unwrap();
    assert!(
        unknown
            .accept(reliable("netsu-other"))
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    assert!(
        unknown
            .accept(reliable(&data_channel_label(1)))
            .unwrap_err()
            .to_string()
            .contains("unexpected payload")
    );
}

#[test]
fn manifest_rejects_wrong_subprotocol_or_unreliable_channels() {
    let mut manifest = ChannelManifest::new(1).unwrap();
    let mut wrong_protocol = reliable(CONTROL_CHANNEL_LABEL);
    wrong_protocol.protocol = "other/1".into();
    assert!(manifest.accept(wrong_protocol).is_err());

    let mut unordered = reliable(CONTROL_CHANNEL_LABEL);
    unordered.ordered = false;
    assert!(manifest.accept(unordered).is_err());

    let mut partial = reliable(CONTROL_CHANNEL_LABEL);
    partial.max_retransmits = Some(1);
    assert!(manifest.accept(partial).is_err());

    let mut externally_negotiated = reliable(CONTROL_CHANNEL_LABEL);
    externally_negotiated.negotiated = true;
    assert!(manifest.accept(externally_negotiated).is_err());
}

fn candidate(kind: CandidateKind) -> SelectedCandidate {
    SelectedCandidate {
        kind,
        protocol: IceProtocol::Udp,
        address: Some("192.0.2.10:5000".into()),
    }
}

#[test]
fn selected_pair_accepts_known_direct_candidate_types_and_redacts_addresses() {
    for local in [
        CandidateKind::Host,
        CandidateKind::ServerReflexive,
        CandidateKind::PeerReflexive,
    ] {
        let pair = SelectedCandidatePair::new(
            candidate(local),
            candidate(CandidateKind::ServerReflexive),
            false,
        )
        .expect("known direct pair");
        assert_eq!(pair.path, "direct");
        assert_eq!(pair.local.address, None);
        assert_eq!(pair.remote.address, None);
    }
}

#[test]
fn selected_pair_rejects_relay_and_unknown_before_payload() {
    for kind in [CandidateKind::Relay, CandidateKind::Unknown] {
        let error =
            SelectedCandidatePair::new(candidate(CandidateKind::Host), candidate(kind), false)
                .unwrap_err();
        assert!(error.to_string().contains("direct path is unavailable"));
    }
}

#[test]
fn selected_pair_includes_addresses_only_when_explicitly_enabled() {
    let pair = SelectedCandidatePair::new(
        candidate(CandidateKind::Host),
        candidate(CandidateKind::Host),
        true,
    )
    .unwrap();
    assert_eq!(pair.local.address.as_deref(), Some("192.0.2.10:5000"));
    assert_eq!(pair.remote.address.as_deref(), Some("192.0.2.10:5000"));
}

async fn forward_all_candidates(from: &mut WebRtcPeer, to: &mut WebRtcPeer) -> usize {
    let mut count = 0;
    loop {
        match from.next_local_ice().await.unwrap() {
            LocalIceEvent::Candidate(candidate) => {
                count += 1;
                to.add_remote_candidate(candidate).await.unwrap();
            }
            LocalIceEvent::Complete => return count,
        }
    }
}

#[tokio::test]
async fn in_process_peers_trickle_candidates_open_control_and_close() {
    let options =
        WebRtcOptions::new("http://127.0.0.1:8787/v1/signal", [] as [&str; 0], false).unwrap();
    let mut offerer = WebRtcPeer::new(&options, PeerRole::Offerer).await.unwrap();
    let mut answerer = WebRtcPeer::new(&options, PeerRole::Answerer).await.unwrap();

    offerer.prepare_control().await.unwrap();
    let offer = offerer.create_offer().await.unwrap();
    // Exercise the bounded candidate queue: trickled candidates may arrive at
    // signaling before the corresponding remote description is applied.
    let offer_candidates = forward_all_candidates(&mut offerer, &mut answerer).await;
    let answer = answerer.accept_offer(offer).await.unwrap();
    offerer.accept_answer(answer).await.unwrap();

    let answer_candidates = forward_all_candidates(&mut answerer, &mut offerer).await;
    assert!(
        offer_candidates > 0,
        "offerer candidate callback did not fire"
    );
    assert!(
        answer_candidates > 0,
        "answerer candidate callback did not fire"
    );

    let (offer_path, answer_path) = tokio::join!(
        offerer.wait_for_direct_path(),
        answerer.wait_for_direct_path()
    );
    assert_eq!(offer_path.unwrap().path, "direct");
    assert_eq!(answer_path.unwrap().path, "direct");

    let (mut outgoing, mut incoming) =
        tokio::join!(offerer.take_prepared_control(), answerer.accept_control());
    let outgoing = outgoing.as_mut().unwrap();
    let incoming = incoming.as_mut().unwrap();
    outgoing.write_all(b"hello").await.unwrap();
    assert_eq!(
        incoming
            .read_exact(5, Some(std::time::Duration::from_secs(2)))
            .await
            .unwrap(),
        b"hello"
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        outgoing.close().await;
        incoming.close().await;
        offerer.close().await.unwrap();
        answerer.close().await.unwrap();
    })
    .await
    .expect("deterministic peer shutdown");
}

#[tokio::test]
async fn answerer_buffers_control_bytes_before_accept_control() {
    let options =
        WebRtcOptions::new("http://127.0.0.1:8787/v1/signal", [] as [&str; 0], false).unwrap();
    let mut offerer = WebRtcPeer::new(&options, PeerRole::Offerer).await.unwrap();
    let mut answerer = WebRtcPeer::new(&options, PeerRole::Answerer).await.unwrap();

    offerer.prepare_control().await.unwrap();
    let offer = offerer.create_offer().await.unwrap();
    forward_all_candidates(&mut offerer, &mut answerer).await;
    let answer = answerer.accept_offer(offer).await.unwrap();
    offerer.accept_answer(answer).await.unwrap();
    forward_all_candidates(&mut answerer, &mut offerer).await;

    let (offer_path, answer_path) = tokio::join!(
        offerer.wait_for_direct_path(),
        answerer.wait_for_direct_path()
    );
    offer_path.unwrap();
    answer_path.unwrap();

    let mut outgoing = offerer.take_prepared_control().await.unwrap();
    let cookie = [0x5au8; 37];
    outgoing.write_all(&cookie).await.unwrap();

    // Production establishes the direct path before handing the control pipe
    // to the protocol. Bytes arriving in that gap must already be buffered.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut incoming = answerer.accept_control().await.unwrap();
    let received = incoming
        .read_exact(cookie.len(), Some(std::time::Duration::from_secs(1)))
        .await
        .expect("control bytes sent before accept_control must be buffered");
    assert_eq!(received, cookie);

    offerer.close().await.unwrap();
    answerer.close().await.unwrap();
}
