#![cfg(feature = "ws")]

use futures_util::SinkExt;
use netsu::transport::ws::WsPipe;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Serve one WS connection, sending the caller-supplied chunks as separate
/// binary messages with a gap between them so each lands as its own frame.
async fn serve_chunks(chunks: Vec<Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        for c in chunks {
            ws.send(Message::Binary(c)).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    });
    port
}

#[tokio::test]
async fn reassembles_one_protocol_unit_split_across_many_ws_messages() {
    let unit: Vec<u8> = (0..37u8).collect();
    // Deliberately unaligned split: 3, 1, 20, 6, 7
    let chunks = vec![
        unit[0..3].to_vec(),
        unit[3..4].to_vec(),
        unit[4..24].to_vec(),
        unit[24..30].to_vec(),
        unit[30..37].to_vec(),
    ];
    let port = serve_chunks(chunks).await;

    let mut pipe = WsPipe::connect("127.0.0.1", port, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(pipe.read_exact(37, None).await.unwrap(), unit);
}

#[tokio::test]
async fn handles_a_message_carrying_the_tail_of_one_unit_and_the_head_of_the_next() {
    let a: Vec<u8> = vec![0xAA; 37];
    let b: Vec<u8> = vec![0xBB; 37];
    // First message: all of A's head. Second: A's tail + B's head. Third: B's tail.
    let chunks = vec![
        a[0..30].to_vec(),
        [&a[30..37], &b[0..10]].concat(),
        b[10..37].to_vec(),
    ];
    let port = serve_chunks(chunks).await;

    let mut pipe = WsPipe::connect("127.0.0.1", port, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(pipe.read_exact(37, None).await.unwrap(), a);
    assert_eq!(pipe.read_exact(37, None).await.unwrap(), b);
}
