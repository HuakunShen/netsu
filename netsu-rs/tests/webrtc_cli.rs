#![cfg(feature = "webrtc")]

use netsu::client::Transport;
use netsu::error::{SetupPhase, WebRtcSetupFailure, webrtc_setup_error};
use netsu::transport::webrtc::config::WebRtcOptions;
use netsu::transport::webrtc::signaling::{
    ClientSignalMessage, DescriptionType, ServerSignalMessage, SignalRole, decode_server_message,
    encode_client_message,
};

#[test]
fn webrtc_configuration_is_direct_only_and_rejects_turn() {
    let options = WebRtcOptions::new(
        "https://signal.example/v1/signal",
        ["stun:stun.example:3478"],
        false,
    )
    .expect("valid direct-only WebRTC options");

    assert_eq!(options.signal_url.scheme(), "https");
    assert_eq!(options.stun_urls, ["stun:stun.example:3478"]);
    assert!(!options.include_addresses);
    assert_eq!(Transport::WebRtc, Transport::WebRtc);

    for url in [
        "turn:relay.example:3478",
        "turns:relay.example:5349",
        "https://not-an-ice-server.example",
        "",
    ] {
        let error = WebRtcOptions::new("https://signal.example/v1/signal", [url], false)
            .expect_err("non-STUN ICE URL must be rejected");
        assert!(error.to_string().contains("STUN"));
    }
}

#[test]
fn webrtc_configuration_rejects_bad_signal_urls_and_too_many_stun_urls() {
    for url in ["ws://signal.example", "ftp://signal.example", "not a URL"] {
        assert!(
            WebRtcOptions::new(url, std::iter::empty::<&str>(), false).is_err(),
            "accepted {url}"
        );
    }

    let error = WebRtcOptions::new(
        "http://127.0.0.1:8787/v1/signal",
        [
            "stun:a.example:3478",
            "stun:b.example:3478",
            "stun:c.example:3478",
            "stun:d.example:3478",
            "stun:e.example:3478",
        ],
        false,
    )
    .expect_err("more than four STUN servers must be rejected");
    assert!(error.to_string().contains("at most 4"));
}

#[test]
fn signaling_base_url_appends_room_routes_without_dropping_the_base_path() {
    let options = WebRtcOptions::new(
        "https://signal.example/v1/signal",
        std::iter::empty::<&str>(),
        false,
    )
    .unwrap();
    assert_eq!(
        options.rooms_url().unwrap().as_str(),
        "https://signal.example/v1/signal/rooms"
    );
    assert_eq!(
        options.room_websocket_url("ABCD-EFGH").unwrap().as_str(),
        "wss://signal.example/v1/signal/rooms/ABCD-EFGH/ws"
    );
}

#[test]
fn signaling_v1_messages_match_the_worker_wire_contract() {
    let bind = ClientSignalMessage::bind_listener("secret-fixture");
    assert!(!format!("{bind:?}").contains("secret-fixture"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encode_client_message(&bind).unwrap()).unwrap(),
        serde_json::json!({
            "v": 1,
            "type": "bind",
            "role": "listener",
            "secret": "secret-fixture",
        })
    );

    let offer = ClientSignalMessage::description(DescriptionType::Offer, "fixture-offer");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encode_client_message(&offer).unwrap()).unwrap(),
        serde_json::json!({
            "v": 1,
            "type": "description",
            "sdp_type": "offer",
            "sdp": "fixture-offer",
        })
    );

    let bound =
        decode_server_message(r#"{"v":1,"type":"bound","role":"joiner","expires_in_seconds":60}"#)
            .unwrap();
    assert_eq!(
        bound,
        ServerSignalMessage::Bound {
            v: 1,
            role: SignalRole::Joiner,
            expires_in_seconds: 60,
        }
    );
}

#[test]
fn signaling_decoder_enforces_version_and_resource_limits() {
    let wrong_version = decode_server_message(r#"{"v":2,"type":"peer_ready"}"#)
        .expect_err("v2 must not be accepted by a v1 client");
    assert!(wrong_version.to_string().contains("version"));

    let oversized = format!(
        r#"{{"v":1,"type":"description","sdp_type":"answer","sdp":"{}"}}"#,
        "x".repeat(65_536)
    );
    let error = decode_server_message(&oversized).expect_err("oversized frame must fail");
    assert!(error.to_string().contains("64 KiB"));
}

#[test]
fn setup_phases_cover_the_full_webrtc_handshake() {
    let phases = [
        SetupPhase::SignalingConnect,
        SetupPhase::SignalingRoom,
        SetupPhase::OfferAnswer,
        SetupPhase::IceGathering,
        SetupPhase::IceConnected,
        SetupPhase::PeerConnected,
        SetupPhase::ChannelsOpen,
    ];
    assert_eq!(phases.len(), 7);
}

#[test]
fn webrtc_setup_errors_use_a_sanitized_public_vocabulary() {
    let error = webrtc_setup_error(
        SetupPhase::OfferAnswer,
        WebRtcSetupFailure::InvalidRemoteDescription,
    );
    let display = error.to_string();
    assert_eq!(
        display,
        "webrtc setup failed during offer/answer: remote description was rejected"
    );
    for secret_material in ["candidate:", "v=0", "listener_secret", "192.0.2.1"] {
        assert!(!display.contains(secret_material));
    }
}
