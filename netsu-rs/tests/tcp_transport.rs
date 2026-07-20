use netsu::protocol::framing::{MAX_JSON, read_json, write_json};
use netsu::transport::tcp::{CONNECT_TIMEOUT, TcpPipe};
use tokio::net::TcpListener;

#[tokio::test]
async fn carries_framed_json_both_ways() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut pipe = TcpPipe::from_stream(sock);
        let msg: serde_json::Value = read_json(&mut pipe, MAX_JSON, None).await.unwrap();
        write_json(&mut pipe, &serde_json::json!({ "echo": msg }))
            .await
            .unwrap();
    });

    let mut pipe = TcpPipe::connect("127.0.0.1", port, CONNECT_TIMEOUT)
        .await
        .unwrap();
    write_json(&mut pipe, &serde_json::json!({ "hello": 1 }))
        .await
        .unwrap();
    let got: serde_json::Value = read_json(&mut pipe, MAX_JSON, None).await.unwrap();
    assert_eq!(got, serde_json::json!({ "echo": { "hello": 1 } }));
    server.await.unwrap();
}

#[tokio::test]
async fn into_data_channel_moves_bulk_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut pipe = TcpPipe::from_stream(sock);
        pipe.read_exact(4, None).await.unwrap(); // handshake stand-in
        pipe.write_all(&[1]).await.unwrap(); // ack, gates the bulk write
        let mut ch = pipe.into_data_channel().unwrap();
        let mut buf = vec![0u8; 65536];
        let mut total = 0usize;
        while total < 65536 {
            match ch.read_chunk(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
        total
    });

    let mut pipe = TcpPipe::connect("127.0.0.1", port, CONNECT_TIMEOUT)
        .await
        .unwrap();
    pipe.write_all(&[1, 2, 3, 4]).await.unwrap();
    pipe.read_exact(1, None).await.unwrap(); // wait for ack
    let mut ch = pipe.into_data_channel().unwrap();
    ch.write_chunk(&vec![7u8; 65536]).await.unwrap();
    ch.close().await;
    assert!(server.await.unwrap() >= 65536);
}

// Ignored in this sandbox: outbound TCP connections are transparently
// intercepted and made to succeed instantly regardless of destination, even
// to TEST-NET-1 (192.0.2.0/24, reserved and non-routable per RFC 5737).
// Verified independently of this test with `nc -v -w 3 192.0.2.1 5310` and a
// second reserved address/port pair, both reporting "succeeded!" in well
// under a second with no proxy environment variables set — i.e. this is a
// network-level property of the sandbox, not something `TcpPipe::connect`
// can be adjusted to detect. The assertion below (`got.is_err()`) fails here
// because the connect succeeds, not because the timeout doesn't fire; on a
// host with normal networking, an unreachable TEST-NET-1 address never
// completes its handshake and this test exercises the real timeout path.
#[tokio::test]
#[ignore = "sandbox intercepts all outbound TCP connections and makes them succeed instantly, even to reserved/non-routable addresses"]
async fn connect_times_out_rather_than_hanging() {
    // TEST-NET-1, reserved and non-routable: the handshake cannot complete.
    let start = std::time::Instant::now();
    let got = TcpPipe::connect("192.0.2.1", 5310, std::time::Duration::from_millis(300)).await;
    assert!(got.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}
