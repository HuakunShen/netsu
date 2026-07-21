#![cfg(feature = "webrtc")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use netsu::protocol::pipe::BytePipe;
use netsu::transport::webrtc::config::WebRtcOptions;
use netsu::transport::webrtc::peer::{negotiate_answerer, negotiate_offerer};
use netsu::transport::webrtc::signaling::{
    ClientSignalMessage, DescriptionType, ServerSignalMessage, SignalingClient,
};
use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const WORKER_PORT: u16 = 18_787;

struct WorkerdGuard {
    child: Child,
}

impl WorkerdGuard {
    async fn start(repo_root: &Path) -> Self {
        let script = repo_root.join("scripts/dev-webrtc-signal.sh");
        let mut child = Command::new(script)
            .current_dir(repo_root)
            .env("SIGNAL_WORKER_PORT", WORKER_PORT.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start Wrangler signaling wrapper");
        let stdout = child.stdout.take().expect("wrapper stdout");
        let mut lines = BufReader::new(stdout).lines();
        let ready = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .expect("Wrangler readiness timed out")
            .expect("read wrapper readiness")
            .expect("wrapper exited before readiness");
        assert_eq!(
            ready,
            format!("READY http://127.0.0.1:{WORKER_PORT}/v1/signal")
        );
        Self { child }
    }

    async fn shutdown(mut self) {
        if let Some(pid) = self.child.id() {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("Wrangler wrapper did not stop")
            .expect("wait for Wrangler wrapper");
    }
}

impl Drop for WorkerdGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("netsu-rs lives under the repository root")
        .to_path_buf()
}

#[tokio::test]
async fn rust_client_exchanges_signaling_v1_through_real_workerd() {
    let worker = WorkerdGuard::start(&repo_root()).await;
    let options = WebRtcOptions::new(
        &format!("http://127.0.0.1:{WORKER_PORT}/v1/signal"),
        std::iter::empty::<&str>(),
        false,
    )
    .unwrap();
    let client = SignalingClient::new(
        options.clone(),
        Some(SecretString::from("local-signal-test-token")),
    );

    let mut listener = client.create_listener(60).await.unwrap();
    assert_eq!(listener.room.code.len(), 9);
    assert!(listener.room.expires_at.contains('T'));

    let mut joiner = client.join(&listener.room.code).await.unwrap();
    let second_joiner = client.join(&listener.room.code).await;
    let second_joiner_error = match second_joiner {
        Ok(_) => panic!("a signaling room accepted a second joiner"),
        Err(error) => error,
    };
    assert_eq!(
        second_joiner_error.to_string(),
        "webrtc setup failed during signaling room: signaling room is unavailable"
    );
    assert!(matches!(
        listener.session.next().await.unwrap(),
        ServerSignalMessage::PeerReady { v: 1 }
    ));
    assert!(matches!(
        joiner.next().await.unwrap(),
        ServerSignalMessage::PeerReady { v: 1 }
    ));

    joiner
        .send(&ClientSignalMessage::description(
            DescriptionType::Offer,
            "workerd-offer-fixture",
        ))
        .await
        .unwrap();
    assert!(matches!(
        listener.session.next().await.unwrap(),
        ServerSignalMessage::Description {
            sdp_type: DescriptionType::Offer,
            ref sdp,
            ..
        } if sdp == "workerd-offer-fixture"
    ));

    listener
        .session
        .send(&ClientSignalMessage::description(
            DescriptionType::Answer,
            "workerd-answer-fixture",
        ))
        .await
        .unwrap();
    assert!(matches!(
        joiner.next().await.unwrap(),
        ServerSignalMessage::Description {
            sdp_type: DescriptionType::Answer,
            ref sdp,
            ..
        } if sdp == "workerd-answer-fixture"
    ));

    joiner
        .send(&ClientSignalMessage::Candidate {
            v: 1,
            candidate: "workerd-candidate-fixture".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: Some("workerd-fragment".into()),
        })
        .await
        .unwrap();
    assert!(matches!(
        listener.session.next().await.unwrap(),
        ServerSignalMessage::Candidate {
            ref candidate,
            sdp_mline_index: Some(0),
            ..
        } if candidate == "workerd-candidate-fixture"
    ));

    for index in 1..16 {
        joiner
            .send(&ClientSignalMessage::Candidate {
                v: 1,
                candidate: format!("workerd-candidate-{index}"),
                sdp_mid: Some("0".into()),
                sdp_mline_index: Some(0),
                username_fragment: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            listener.session.next().await.unwrap(),
            ServerSignalMessage::Candidate { .. }
        ));
    }
    let candidate_limit = joiner
        .send(&ClientSignalMessage::Candidate {
            v: 1,
            candidate: "seventeenth-candidate".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: None,
        })
        .await
        .expect_err("the client must reject candidate 17 before sending it");
    assert_eq!(
        candidate_limit.to_string(),
        "protocol error: signaling candidate limit exceeded"
    );

    joiner.leave().await.unwrap();
    assert!(matches!(
        listener.session.next().await.unwrap(),
        ServerSignalMessage::PeerLeft { v: 1 }
    ));
    listener.session.close().await.unwrap();

    let mut listener = client.create_listener(60).await.unwrap();
    let mut joiner = client.join(&listener.room.code).await.unwrap();
    let (answerer, offerer) = tokio::join!(
        negotiate_answerer(&options, &mut listener.session),
        negotiate_offerer(&options, &mut joiner),
    );
    let mut answerer = answerer.unwrap();
    let mut offerer = offerer.unwrap();
    assert_eq!(answerer.metrics.selected_pair.path, "direct");
    assert_eq!(offerer.metrics.selected_pair.path, "direct");
    assert!(answerer.metrics.offer_answer_ms.is_finite());
    assert!(offerer.metrics.channels_open_ms.is_finite());

    offerer.control.write_all(b"workerd-webrtc").await.unwrap();
    assert_eq!(
        answerer
            .control
            .read_exact(b"workerd-webrtc".len(), Some(Duration::from_secs(2)))
            .await
            .unwrap(),
        b"workerd-webrtc"
    );
    let (_, _) = tokio::join!(offerer.control.close(), answerer.control.close());
    let (offerer_close, answerer_close) = tokio::join!(offerer.peer.close(), answerer.peer.close());
    offerer_close.unwrap();
    answerer_close.unwrap();
    worker.shutdown().await;
}
