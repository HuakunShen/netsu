//! Control-channel framing: single signed state bytes, and length-prefixed
//! JSON messages (`JSON_write`/`JSON_read` in `iperf_api.c`).

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::pipe::BytePipe;
use crate::error::{NetsuError, Result};

/// netsu's cap on all JSON reads. iperf3 itself caps `PARAM_EXCHANGE` at 8
/// KiB (`MAX_PARAMS_JSON_STRING`); see `PROTOCOL.md`'s "JSON framing".
pub const MAX_JSON: usize = 65536;

/// Writes a single signed state byte (iperf3 writes states as one byte on
/// the control channel).
pub async fn write_state<P: BytePipe>(p: &mut P, state: i8) -> Result<()> {
    p.write_all(&[state as u8]).await
}

/// Reads a single state byte back, sign-extending it — `0xFF` on the wire
/// must come back as `-1`, not `255`.
pub async fn read_state<P: BytePipe>(p: &mut P, timeout: Option<Duration>) -> Result<i8> {
    let b = p.read_exact(1, timeout).await?;
    Ok(b[0] as i8)
}

/// `[4-byte unsigned big-endian length][UTF-8 JSON bytes]`.
pub async fn write_json<P: BytePipe, T: Serialize>(p: &mut P, v: &T) -> Result<()> {
    let body = serde_json::to_vec(v)?;
    let len = u32::try_from(body.len())
        .map_err(|_| NetsuError::Protocol(format!("json body too large: {} bytes", body.len())))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&body);
    p.write_all(&frame).await
}

pub async fn read_json<P: BytePipe, T: DeserializeOwned>(
    p: &mut P,
    max: usize,
    timeout: Option<Duration>,
) -> Result<T> {
    let head = p.read_exact(4, timeout).await?;
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if size == 0 || size > max {
        return Err(NetsuError::Protocol(format!(
            "json frame too large: {size} > {max}"
        )));
    }
    let body = p.read_exact(size, timeout).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::pipe::MemoryPipe;
    use crate::protocol::states::{ACCESS_DENIED, PARAM_EXCHANGE};

    #[tokio::test]
    async fn round_trips_positive_and_negative_state_bytes() {
        let (mut a, mut b) = MemoryPipe::pair();
        write_state(&mut a, PARAM_EXCHANGE).await.unwrap();
        write_state(&mut a, ACCESS_DENIED).await.unwrap();
        assert_eq!(read_state(&mut b, None).await.unwrap(), PARAM_EXCHANGE);
        // signed: 0xff must read back as -1, not 255
        assert_eq!(read_state(&mut b, None).await.unwrap(), ACCESS_DENIED);
    }

    #[tokio::test]
    async fn round_trips_json_with_4_byte_be_length_prefix() {
        let (mut a, mut b) = MemoryPipe::pair();
        let msg = serde_json::json!({ "tcp": true, "time": 10, "parallel": 2 });
        write_json(&mut a, &msg).await.unwrap();
        let got: serde_json::Value = read_json(&mut b, MAX_JSON, None).await.unwrap();
        assert_eq!(got, msg);
    }

    #[tokio::test]
    async fn rejects_json_larger_than_max() {
        let (mut a, mut b) = MemoryPipe::pair();
        let msg = serde_json::json!({ "pad": "x".repeat(100) });
        write_json(&mut a, &msg).await.unwrap();
        let got = read_json::<_, serde_json::Value>(&mut b, 50, None).await;
        assert!(got.is_err());
    }
}
