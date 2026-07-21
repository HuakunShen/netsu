//! The mux wire protocol (netsu-internal, not iperf3). One iroh connection
//! carries a control bi-stream plus one bi-stream per data stream. A measured
//! ("probe") stream echoes each sequence number back on its own reverse
//! channel, so RTT is measured per-stream without a shared ACK channel.
//!
//! Control frames + stream hellos are length-prefixed postcard. Data messages
//! use a fixed 13-byte binary header (hot path) + payload; a probe echo is 8
//! bytes (the sequence number).

use anyhow::{Context, bail};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::mux::config::WorkloadKind;

pub const MUX_ALPN: &[u8] = b"netsu/mux/1";
pub const PROTOCOL_VERSION: u16 = 1;
/// Cap on a length-prefixed control/hello frame, guarding allocation.
pub const MAX_FRAME: usize = 256 * 1024;
/// Fixed data-message header: `[seq: u64 LE][flags: u8][len: u32 LE]`.
pub const DATA_HEADER_LEN: usize = 13;
/// `flags` bit: this message was sent inside the measured window.
pub const FLAG_MEASURED_WINDOW: u8 = 0x01;

/// Sent first on the control bi-stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Start {
    pub version: u16,
    pub run_id: Uuid,
    pub stream_count: u16,
}

/// Sent first on every data bi-stream, identifying it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StreamHello {
    pub version: u16,
    pub run_id: Uuid,
    pub kind: WorkloadKind,
    pub index: u16,
    pub measured: bool,
}

/// Control frames after the initial `Start`/`Ready` handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum Control {
    Ready,
    Stop,
    /// The receiver's byte tally per stream index, for reconciliation.
    Summary { received: Vec<(u16, u64)> },
}

/// Write a length-prefixed postcard frame.
pub async fn write_frame<W, T>(w: &mut W, value: &T) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = postcard::to_allocvec(value).context("encode mux frame")?;
    if body.len() > MAX_FRAME {
        bail!("mux frame too large: {} bytes", body.len());
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

/// Read a length-prefixed postcard frame, rejecting oversize before allocating.
pub async fn read_frame<R, T>(r: &mut R) -> anyhow::Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await.context("read frame length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        bail!("mux frame length {len} exceeds cap {MAX_FRAME}");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.context("read frame body")?;
    postcard::from_bytes(&body).context("decode mux frame")
}

/// Write one data message header + payload.
pub async fn write_data<W: AsyncWrite + Unpin>(
    w: &mut W,
    seq: u64,
    measured_window: bool,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut header = [0u8; DATA_HEADER_LEN];
    header[0..8].copy_from_slice(&seq.to_le_bytes());
    header[8] = if measured_window { FLAG_MEASURED_WINDOW } else { 0 };
    header[9..13].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&header).await?;
    w.write_all(payload).await?;
    Ok(())
}

/// A decoded data-message header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeader {
    pub seq: u64,
    pub measured_window: bool,
    pub len: usize,
}

/// Read one data message header. Returns `None` on a clean end-of-stream.
pub async fn read_data_header<R: AsyncRead + Unpin>(
    r: &mut R,
) -> anyhow::Result<Option<DataHeader>> {
    let mut header = [0u8; DATA_HEADER_LEN];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("read data header"),
    }
    let seq = u64::from_le_bytes(header[0..8].try_into().unwrap());
    let measured_window = header[8] & FLAG_MEASURED_WINDOW != 0;
    let len = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;
    Ok(Some(DataHeader { seq, measured_window, len }))
}

/// Echo a sequence number back on a probe stream's reverse channel.
pub async fn write_echo<W: AsyncWrite + Unpin>(w: &mut W, seq: u64) -> anyhow::Result<()> {
    w.write_all(&seq.to_le_bytes()).await?;
    Ok(())
}

/// Read one 8-byte echo. Returns `None` on clean end-of-stream.
pub async fn read_echo<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Option<u64>> {
    let mut buf = [0u8; 8];
    match r.read_exact(&mut buf).await {
        Ok(_) => Ok(Some(u64::from_le_bytes(buf))),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e).context("read echo"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_frame_round_trips() {
        let start = Start { version: PROTOCOL_VERSION, run_id: Uuid::from_u128(1), stream_count: 3 };
        let mut buf = Vec::new();
        write_frame(&mut buf, &start).await.unwrap();
        let mut slice = &buf[..];
        let got: Start = read_frame(&mut slice).await.unwrap();
        assert_eq!(got, start);
    }

    #[tokio::test]
    async fn data_message_round_trips_with_measured_flag() {
        let mut buf = Vec::new();
        write_data(&mut buf, 42, true, b"hello").await.unwrap();
        let mut slice = &buf[..];
        let h = read_data_header(&mut slice).await.unwrap().unwrap();
        assert_eq!(h.seq, 42);
        assert!(h.measured_window);
        assert_eq!(h.len, 5);
        let mut payload = vec![0u8; h.len];
        slice.read_exact(&mut payload).await.unwrap();
        assert_eq!(payload, b"hello");
    }

    #[tokio::test]
    async fn clean_eof_yields_none() {
        let empty: &[u8] = &[];
        let mut slice = empty;
        assert!(read_data_header(&mut slice).await.unwrap().is_none());
        let mut slice2 = empty;
        assert!(read_echo(&mut slice2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversize_frame_length_is_rejected_before_alloc() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(u32::MAX).to_le_bytes());
        let mut slice = &buf[..];
        let got: Result<Start, _> = read_frame(&mut slice).await;
        assert!(got.is_err());
    }

    #[tokio::test]
    async fn echo_round_trips() {
        let mut buf = Vec::new();
        write_echo(&mut buf, 7).await.unwrap();
        let mut slice = &buf[..];
        assert_eq!(read_echo(&mut slice).await.unwrap(), Some(7));
    }
}
