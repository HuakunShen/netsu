//! iroh transport: the iperf3 control state machine and bulk data streams,
//! multiplexed as separate QUIC bi-directional streams over one iroh
//! `Connection`.
//!
//! Unlike TCP/WS — where the control channel and each data stream are separate
//! OS connections — an iroh test uses ONE connection carrying many bi streams:
//! the control stream (an [`IrohPipe`], `BytePipe`) plus N data streams (each
//! an [`IrohChannel`], `DataChannel`). There is therefore no
//! `into_data_channel` hand-off: streams are opened independently with
//! `Connection::open_bi` / accepted with `Connection::accept_bi`.

use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};

use crate::error::{NetsuError, Result};
use crate::protocol::pipe::BytePipe;
use crate::streams::channel::DataChannel;

fn send_err(e: impl std::fmt::Display) -> NetsuError {
    NetsuError::Protocol(format!("iroh send: {e}"))
}

fn recv_err(e: impl std::fmt::Display) -> NetsuError {
    NetsuError::Protocol(format!("iroh recv: {e}"))
}

/// Control-channel view of one iroh bi stream: the cookie/state/JSON handshake
/// runs over this via the generic `BytePipe` machinery.
pub struct IrohPipe {
    send: SendStream,
    recv: RecvStream,
}

impl IrohPipe {
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        IrohPipe { send, recv }
    }

    // Inherent forwarders, mirroring `TcpPipe`, so a caller holding a concrete
    // `IrohPipe` need not import `BytePipe` to use it.
    pub async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        <Self as BytePipe>::read_exact(self, n, timeout).await
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        <Self as BytePipe>::write_all(self, data).await
    }

    pub async fn close(&mut self) {
        <Self as BytePipe>::close(self).await
    }
}

impl BytePipe for IrohPipe {
    async fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let recv = &mut self.recv;
        let fill = async move {
            let mut buf = vec![0u8; n];
            match recv.read_exact(&mut buf).await {
                Ok(()) => Ok(buf),
                // Stream ended before `n` bytes arrived — the pipe is closed.
                Err(ReadExactError::FinishedEarly { .. }) => Err(NetsuError::PipeClosed),
                Err(e) => Err(recv_err(e)),
            }
        };
        match timeout {
            Some(d) => tokio::time::timeout(d, fill)
                .await
                .map_err(|_| NetsuError::Timeout)?,
            None => fill.await,
        }
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        // `SendStream::write_all` resolves once QUIC has accepted the bytes —
        // the backpressure point, same contract as the TCP path.
        self.send.write_all(data).await.map_err(send_err)
    }

    async fn close(&mut self) {
        // Best-effort: signal end-of-stream on the send half. A `finish` on an
        // already-closed stream is not actionable.
        let _ = self.send.finish();
    }
}

/// Bulk payload channel over one iroh bi stream.
pub struct IrohChannel {
    send: SendStream,
    recv: RecvStream,
    /// First failure, latched so `error()` reports it at result-finalization
    /// time even for a reader loop that only branches on `Ok(0) | Err(_)` and
    /// discards the specific error. Same rationale as `TcpDataChannel`.
    error: Option<NetsuError>,
}

impl IrohChannel {
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        IrohChannel {
            send,
            recv,
            error: None,
        }
    }

    fn latch(&mut self, err: &NetsuError) {
        self.error
            .get_or_insert_with(|| NetsuError::Protocol(err.to_string()));
    }

    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        <Self as DataChannel>::write_chunk(self, chunk).await
    }

    pub async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        <Self as DataChannel>::read_chunk(self, buf).await
    }

    pub async fn close(&mut self) {
        <Self as DataChannel>::close(self).await
    }
}

#[async_trait]
impl DataChannel for IrohChannel {
    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        if let Some(err) = &self.error {
            return Err(NetsuError::Protocol(err.to_string()));
        }
        match self.send.write_all(chunk).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let err = send_err(e);
                self.latch(&err);
                Err(err)
            }
        }
    }

    async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        if let Some(err) = &self.error {
            return Err(NetsuError::Protocol(err.to_string()));
        }
        match self.recv.read(buf).await {
            Ok(Some(n)) => Ok(n),
            Ok(None) => Ok(0), // clean end of stream
            Err(e) => {
                let err = recv_err(e);
                self.latch(&err);
                Err(err)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p2p::THROUGHPUT_ALPN;
    use crate::p2p::endpoint::LocalPair;

    #[tokio::test]
    async fn iroh_pipe_read_exact_respects_boundaries_and_close() {
        let pair = LocalPair::connect(THROUGHPUT_ALPN).await.unwrap();
        let server = pair.server_connection.clone();
        let srv = tokio::spawn(async move {
            let (send, recv) = server.accept_bi().await.unwrap();
            let mut pipe = IrohPipe::new(send, recv);
            // Read 2 then 3 bytes out of a single 5-byte write.
            assert_eq!(pipe.read_exact(2, None).await.unwrap(), vec![1, 2]);
            assert_eq!(pipe.read_exact(3, None).await.unwrap(), vec![3, 4, 5]);
            // Peer closes; next read_exact must surface PipeClosed.
            let err = pipe.read_exact(1, None).await.unwrap_err();
            assert!(matches!(err, NetsuError::PipeClosed));
        });

        let (send, recv) = pair.client_connection.open_bi().await.unwrap();
        let mut pipe = IrohPipe::new(send, recv);
        pipe.write_all(&[1, 2, 3, 4, 5]).await.unwrap();
        pipe.close().await;

        srv.await.unwrap();
        pair.close().await;
    }

    #[tokio::test]
    async fn iroh_channel_round_trips_chunks_and_reports_eof() {
        let pair = LocalPair::connect(THROUGHPUT_ALPN).await.unwrap();
        let server = pair.server_connection.clone();
        let srv = tokio::spawn(async move {
            let (send, recv) = server.accept_bi().await.unwrap();
            let mut ch = IrohChannel::new(send, recv);
            let mut buf = [0u8; 16];
            let mut total = 0;
            loop {
                let n = ch.read_chunk(&mut buf).await.unwrap();
                if n == 0 {
                    break; // clean EOF
                }
                total += n;
            }
            assert_eq!(total, 10);
            assert!(ch.error().is_none());
        });

        let (send, recv) = pair.client_connection.open_bi().await.unwrap();
        let mut ch = IrohChannel::new(send, recv);
        ch.write_chunk(&[0u8; 10]).await.unwrap();
        ch.close().await;

        srv.await.unwrap();
        pair.close().await;
    }
}
