#![cfg(feature = "webrtc")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use netsu::error::{NetsuError, Result};
use netsu::protocol::pipe::BytePipe;
use netsu::streams::channel::DataChannel;
use netsu::transport::webrtc::channel::WebRtcChannel;
use netsu::transport::webrtc::pipe::{
    DataChannelSink, MAX_DATA_CHANNEL_MESSAGE_BYTES, RECEIVE_QUEUE_LIMIT, SEND_HIGH_WATERMARK,
    SEND_LOW_WATERMARK, WebRtcPipe,
};
use tokio::sync::{Mutex, Notify};

#[derive(Default)]
struct FakeSink {
    sent: Mutex<Vec<Vec<u8>>>,
    buffered: AtomicUsize,
    low_threshold: AtomicUsize,
    low: Notify,
    closed: AtomicBool,
}

impl FakeSink {
    fn with_buffered(bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            buffered: AtomicUsize::new(bytes),
            ..Self::default()
        })
    }

    fn drain_to(&self, bytes: usize) {
        self.buffered.store(bytes, Ordering::SeqCst);
        if bytes <= self.low_threshold.load(Ordering::SeqCst) {
            self.low.notify_waiters();
        }
    }
}

#[async_trait]
impl DataChannelSink for FakeSink {
    async fn send_binary(&self, data: &[u8]) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(NetsuError::PipeClosed);
        }
        self.sent.lock().await.push(data.to_vec());
        self.buffered.fetch_add(data.len(), Ordering::SeqCst);
        Ok(())
    }

    async fn buffered_amount(&self) -> usize {
        self.buffered.load(Ordering::SeqCst)
    }

    async fn set_buffered_amount_low_threshold(&self, bytes: usize) {
        self.low_threshold.store(bytes, Ordering::SeqCst);
    }

    async fn wait_buffered_amount_at_most(&self, maximum: usize) {
        loop {
            let notified = self.low.notified();
            if self.buffered.load(Ordering::SeqCst) <= maximum {
                return;
            }
            notified.await;
        }
    }

    async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn pipe_read_exact_spans_and_splits_message_boundaries() {
    let sink = Arc::new(FakeSink::default());
    let (mut pipe, inbound) = WebRtcPipe::new(sink);
    inbound.feed_binary(b"ab").await.unwrap();
    inbound.feed_binary(b"cdef").await.unwrap();

    assert_eq!(pipe.read_exact(4, None).await.unwrap(), b"abcd");
    assert_eq!(pipe.read_exact(2, None).await.unwrap(), b"ef");
}

#[tokio::test]
async fn pipe_drains_buffered_bytes_before_reporting_close() {
    let sink = Arc::new(FakeSink::default());
    let (mut pipe, inbound) = WebRtcPipe::new(sink);
    inbound.feed_binary(b"abc").await.unwrap();
    inbound.close().await;

    assert_eq!(pipe.read_exact(3, None).await.unwrap(), b"abc");
    assert!(matches!(
        pipe.read_exact(1, None).await,
        Err(NetsuError::PipeClosed)
    ));
}

#[tokio::test]
async fn inbound_rejects_text_and_more_than_one_mibibyte() {
    let sink = Arc::new(FakeSink::default());
    let (_pipe, inbound) = WebRtcPipe::new(sink);
    assert!(inbound.feed_text("not binary").await.is_err());

    let sink = Arc::new(FakeSink::default());
    let (_pipe, inbound) = WebRtcPipe::new(sink);
    inbound
        .feed_binary(&vec![0; RECEIVE_QUEUE_LIMIT])
        .await
        .unwrap();
    let error = inbound
        .feed_binary(&[1])
        .await
        .expect_err("receive queue must remain bounded");
    assert!(error.to_string().contains("receive queue"));
}

#[tokio::test]
async fn writes_are_fragmented_to_the_v1_message_cap() {
    let sink = Arc::new(FakeSink::default());
    let (mut pipe, _inbound) = WebRtcPipe::new(sink.clone());
    let payload = vec![7; MAX_DATA_CHANNEL_MESSAGE_BYTES * 2 + 123];
    pipe.write_all(&payload).await.unwrap();

    let frames = sink.sent.lock().await;
    assert_eq!(frames.len(), 3);
    assert!(
        frames
            .iter()
            .all(|frame| frame.len() <= MAX_DATA_CHANNEL_MESSAGE_BYTES)
    );
    assert_eq!(frames.concat(), payload);
}

#[tokio::test]
async fn backpressure_waits_for_the_low_event_without_polling() {
    let sink = FakeSink::with_buffered(SEND_HIGH_WATERMARK);
    let (mut pipe, _inbound) = WebRtcPipe::new(sink.clone());
    let write = tokio::spawn(async move { pipe.write_all(b"payload").await });
    tokio::task::yield_now().await;
    assert!(sink.sent.lock().await.is_empty());
    assert_eq!(
        sink.low_threshold.load(Ordering::SeqCst),
        SEND_LOW_WATERMARK
    );

    sink.drain_to(SEND_LOW_WATERMARK);
    write.await.unwrap().unwrap();
    assert_eq!(sink.sent.lock().await.as_slice(), &[b"payload".to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn drain_timeout_is_bounded_and_reported() {
    let sink = FakeSink::with_buffered(1);
    let (channel, _inbound) = WebRtcChannel::new(sink);
    let drain = tokio::spawn(async move { channel.drain().await });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    let error = drain
        .await
        .unwrap()
        .expect_err("buffered bytes that never drain must fail");
    assert!(error.to_string().contains("drain timed out"));
}

#[tokio::test]
async fn bulk_channel_reads_fragments_and_rejects_writes_after_close() {
    let sink = Arc::new(FakeSink::default());
    let (mut channel, inbound) = WebRtcChannel::new(sink.clone());
    inbound.feed_binary(b"abcdef").await.unwrap();
    let mut first = [0u8; 4];
    let mut second = [0u8; 4];
    assert_eq!(channel.read_chunk(&mut first).await.unwrap(), 4);
    assert_eq!(&first, b"abcd");
    assert_eq!(channel.read_chunk(&mut second).await.unwrap(), 2);
    assert_eq!(&second[..2], b"ef");

    channel.close().await;
    assert!(channel.write_chunk(b"late").await.is_err());
    assert!(channel.error().is_some());
}

#[tokio::test]
async fn payload_empty_binary_is_an_ordered_end_of_stream_marker() {
    let sink = Arc::new(FakeSink::default());
    let (mut channel, inbound) = WebRtcChannel::new(sink);
    inbound.feed_binary(b"tail").await.unwrap();
    inbound.feed_binary(&[]).await.unwrap();

    let mut target = [0u8; 8];
    assert_eq!(channel.read_chunk(&mut target).await.unwrap(), 4);
    assert_eq!(&target[..4], b"tail");
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            channel.read_chunk(&mut target),
        )
        .await
        .expect("ordered EOF marker was not observed")
        .unwrap(),
        0
    );
}
