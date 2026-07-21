//! The mux receiver: serves one connection — accepts the control stream and N
//! data streams, drains each (echoing sequence numbers back on measured
//! streams), and reports per-stream received-byte tallies.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, ensure};
use iroh::endpoint::{Connection, RecvStream, SendStream};

use crate::mux::protocol::{
    Control, DATA_HEADER_LEN, PROTOCOL_VERSION, Start, StreamHello, read_data_header, read_frame,
    read_payload, write_echo, write_frame,
};

/// Serve one mux connection to completion.
pub async fn serve(connection: Connection) -> anyhow::Result<()> {
    let (mut control_send, mut control_recv) = connection
        .accept_bi()
        .await
        .context("accept mux control stream")?;
    let start: Start = read_frame(&mut control_recv).await.context("read Start")?;
    ensure!(
        start.version == PROTOCOL_VERSION,
        "unsupported mux protocol version {}",
        start.version
    );
    write_frame(&mut control_send, &Control::Ready).await?;

    // Received-byte tally per stream index (measured window only).
    let counters: Arc<Vec<AtomicU64>> =
        Arc::new((0..start.stream_count).map(|_| AtomicU64::new(0)).collect());

    let mut readers = Vec::new();
    for _ in 0..start.stream_count {
        let (send, mut recv) = connection
            .accept_bi()
            .await
            .context("accept mux data stream")?;
        let hello: StreamHello = read_frame(&mut recv).await.context("read StreamHello")?;
        ensure!(
            hello.run_id == start.run_id,
            "data stream from a different run"
        );
        let counters = counters.clone();
        readers.push(tokio::spawn(async move {
            drain_stream(send, recv, hello, counters).await
        }));
    }

    // The client sends Stop once its data streams have finished.
    let _stop: Control = read_frame(&mut control_recv).await.context("read Stop")?;
    for r in readers {
        let _ = r.await;
    }

    let received: Vec<(u16, u64)> = counters
        .iter()
        .enumerate()
        .map(|(i, c)| (i as u16, c.load(Ordering::Relaxed)))
        .collect();
    write_frame(&mut control_send, &Control::Summary { received }).await?;
    Ok(())
}

/// Drain one data stream: read every message (counting measured-window bytes),
/// and on a measured stream echo each measured-window sequence number back.
async fn drain_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    hello: StreamHello,
    counters: Arc<Vec<AtomicU64>>,
) -> anyhow::Result<()> {
    let mut payload = Vec::new();
    while let Some(header) = read_data_header(&mut recv).await? {
        payload.resize(header.len, 0);
        read_payload(&mut recv, &mut payload).await?;
        if header.measured_window {
            if let Some(c) = counters.get(hello.index as usize) {
                c.fetch_add((DATA_HEADER_LEN + header.len) as u64, Ordering::Relaxed);
            }
            if hello.measured {
                write_echo(&mut send, header.seq).await?;
            }
        }
    }
    let _ = send.finish();
    Ok(())
}
