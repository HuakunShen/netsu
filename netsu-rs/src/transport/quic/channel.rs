//! Adapters from Quinn bidirectional streams to netsu control and data traits.

use std::time::Duration;

use async_trait::async_trait;
use quinn::{RecvStream, SendStream};

use crate::error::{NetsuError, Result};
use crate::protocol::pipe::BytePipe;
use crate::streams::channel::DataChannel;

use super::endpoint::DRAIN_TIMEOUT;

fn send_error(error: impl std::fmt::Display) -> NetsuError {
    NetsuError::Protocol(format!("quic send: {error}"))
}

fn receive_error(error: impl std::fmt::Display) -> NetsuError {
    NetsuError::Protocol(format!("quic receive: {error}"))
}

/// Control-channel view of one Quinn bidirectional stream.
pub struct QuicPipe {
    send: SendStream,
    receive: RecvStream,
}

impl QuicPipe {
    pub fn new(send: SendStream, receive: RecvStream) -> Self {
        Self { send, receive }
    }

    pub async fn read_exact(
        &mut self,
        length: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>> {
        <Self as BytePipe>::read_exact(self, length, timeout).await
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        <Self as BytePipe>::write_all(self, data).await
    }

    pub async fn close(&mut self) {
        <Self as BytePipe>::close(self).await
    }

    pub fn into_data_channel(self) -> QuicChannel {
        QuicChannel::new(self.send, self.receive)
    }
}

impl BytePipe for QuicPipe {
    async fn read_exact(&mut self, length: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let receive = &mut self.receive;
        let read = async move {
            let mut buffer = vec![0u8; length];
            match receive.read_exact(&mut buffer).await {
                Ok(()) => Ok(buffer),
                Err(quinn::ReadExactError::FinishedEarly(_)) => Err(NetsuError::PipeClosed),
                Err(error) => Err(receive_error(error)),
            }
        };
        match timeout {
            Some(duration) => tokio::time::timeout(duration, read)
                .await
                .map_err(|_| NetsuError::Timeout)?,
            None => read.await,
        }
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.send.write_all(data).await.map_err(send_error)
    }

    async fn close(&mut self) {
        if self.send.finish().is_ok() {
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, self.send.stopped()).await;
        }
    }
}

/// Bulk-payload view of one Quinn bidirectional stream.
pub struct QuicChannel {
    send: SendStream,
    receive: RecvStream,
    error: Option<NetsuError>,
}

impl QuicChannel {
    pub fn new(send: SendStream, receive: RecvStream) -> Self {
        Self {
            send,
            receive,
            error: None,
        }
    }

    fn latch(&mut self, error: &NetsuError) {
        self.error
            .get_or_insert_with(|| NetsuError::Protocol(error.to_string()));
    }

    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        <Self as DataChannel>::write_chunk(self, chunk).await
    }

    pub async fn read_chunk(&mut self, buffer: &mut [u8]) -> Result<usize> {
        <Self as DataChannel>::read_chunk(self, buffer).await
    }

    pub async fn close(&mut self) {
        <Self as DataChannel>::close(self).await
    }
}

#[async_trait]
impl DataChannel for QuicChannel {
    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        if let Some(error) = &self.error {
            return Err(NetsuError::Protocol(error.to_string()));
        }
        match self.send.write_all(chunk).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let error = send_error(error);
                self.latch(&error);
                Err(error)
            }
        }
    }

    async fn read_chunk(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(error) = &self.error {
            return Err(NetsuError::Protocol(error.to_string()));
        }
        match self.receive.read(buffer).await {
            Ok(Some(length)) => Ok(length),
            Ok(None) => Ok(0),
            Err(error) => {
                let error = receive_error(error);
                self.latch(&error);
                Err(error)
            }
        }
    }

    async fn close(&mut self) {
        let _ = self.send.finish();
    }

    fn error(&self) -> Option<&NetsuError> {
        self.error.as_ref()
    }
}
