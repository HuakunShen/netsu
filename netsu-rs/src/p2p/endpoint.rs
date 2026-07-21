//! iroh endpoint lifecycle: binding listener/client endpoints, connecting,
//! accepting, and an in-process pair for tests. Endpoint setup uses `anyhow`
//! (it is off the `BytePipe`/`DataChannel` hot path); the transport impls in
//! `transport::iroh` map iroh stream errors into `NetsuError`.

use anyhow::Context;
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{Connection, QuicTransportConfig, presets},
};
use iroh_tickets::endpoint::EndpointTicket;

/// QUIC transport config. `send_fairness(true)` round-robins equal-priority
/// streams instead of draining them in open order — the fair default for a
/// multiplexed connection.
pub fn quic_config(send_fairness: bool) -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .send_fairness(send_fairness)
        .build()
}

/// Bind a listening endpoint advertising `alpn`. `direct_only` selects the
/// `Minimal` preset (relay + address discovery off — LAN/loopback only);
/// otherwise `N0` (relay + discovery on, iroh picks direct or relay).
pub async fn bind_listener(
    alpn: &[u8],
    direct_only: bool,
    send_fairness: bool,
) -> anyhow::Result<Endpoint> {
    let transport = quic_config(send_fairness);
    if direct_only {
        Endpoint::builder(presets::Minimal)
            .alpns(vec![alpn.to_vec()])
            .transport_config(transport)
            .bind()
            .await
            .context("bind direct-only iroh listener")
    } else {
        Endpoint::builder(presets::N0)
            .alpns(vec![alpn.to_vec()])
            .transport_config(transport)
            .bind()
            .await
            .context("bind routable iroh listener")
    }
}

/// Encode an endpoint address as a shareable `EndpointTicket` string.
pub fn ticket_for(addr: EndpointAddr) -> String {
    EndpointTicket::new(addr).to_string()
}

/// Parse a peer `EndpointTicket` string into an address to dial.
pub fn parse_ticket(s: &str) -> anyhow::Result<EndpointAddr> {
    let ticket: EndpointTicket = s
        .trim()
        .parse()
        .context("parse endpoint ticket from --peer")?;
    Ok(ticket.into())
}

/// Bind a listener and return it together with a dialable ticket string. For a
/// routable (non-`direct_only`) endpoint, waits until the endpoint is online so
/// the ticket carries a relay URL; a direct-only endpoint is dialable from its
/// socket addresses immediately.
pub async fn bind_listener_with_ticket(
    alpn: &[u8],
    direct_only: bool,
    send_fairness: bool,
) -> anyhow::Result<(Endpoint, String)> {
    let endpoint = bind_listener(alpn, direct_only, send_fairness).await?;
    if !direct_only {
        endpoint.online().await;
    }
    let ticket = ticket_for(endpoint.addr());
    Ok((endpoint, ticket))
}

/// Bind a client endpoint (advertises no ALPN — it only dials).
pub async fn bind_client(direct_only: bool, send_fairness: bool) -> anyhow::Result<Endpoint> {
    let transport = quic_config(send_fairness);
    if direct_only {
        Endpoint::builder(presets::Minimal)
            .transport_config(transport)
            .bind()
            .await
            .context("bind direct-only iroh client")
    } else {
        Endpoint::builder(presets::N0)
            .transport_config(transport)
            .bind()
            .await
            .context("bind routable iroh client")
    }
}

/// Dial `peer` over `alpn`.
pub async fn connect(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    alpn: &[u8],
) -> anyhow::Result<Connection> {
    endpoint
        .connect(peer, alpn)
        .await
        .context("connect iroh peer")
}

/// Accept the next incoming connection on a listener.
pub async fn accept(endpoint: &Endpoint) -> anyhow::Result<Connection> {
    let incoming = endpoint
        .accept()
        .await
        .context("iroh listener closed before a connection arrived")?;
    incoming.await.context("accept iroh connection")
}

/// A connected pair of endpoints in one process, for local smoke tests. Both
/// endpoints are `direct_only` (loopback), so no relay or discovery is used.
pub struct LocalPair {
    pub client_endpoint: Endpoint,
    pub server_endpoint: Endpoint,
    pub client_connection: Connection,
    pub server_connection: Connection,
}

impl LocalPair {
    pub async fn connect(alpn: &'static [u8]) -> anyhow::Result<Self> {
        let server_endpoint = bind_listener(alpn, true, true).await?;
        let client_endpoint = bind_client(true, true).await?;
        let server_for_accept = server_endpoint.clone();
        let accept_fut = async move { accept(&server_for_accept).await };
        let connect_fut = connect(&client_endpoint, server_endpoint.addr(), alpn);
        let (server_connection, client_connection) = tokio::join!(accept_fut, connect_fut);
        Ok(Self {
            client_endpoint,
            server_endpoint,
            client_connection: client_connection.context("local pair: connect")?,
            server_connection: server_connection.context("local pair: accept")?,
        })
    }

    pub async fn close(self) {
        self.client_connection.close(0u32.into(), b"done");
        self.server_connection.close(0u32.into(), b"done");
        let ((), ()) = tokio::join!(self.client_endpoint.close(), self.server_endpoint.close());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p2p::THROUGHPUT_ALPN;

    #[tokio::test]
    async fn local_pair_connects_and_exchanges_a_bidi_message() {
        let pair = LocalPair::connect(THROUGHPUT_ALPN).await.unwrap();

        // Client opens a bi stream and writes; server accepts and echoes length.
        let client = pair.client_connection.clone();
        let server = pair.server_connection.clone();
        let server_task = tokio::spawn(async move {
            let (mut send, mut recv) = server.accept_bi().await.unwrap();
            let got = recv.read_to_end(64).await.unwrap();
            send.write_all(&[got.len() as u8]).await.unwrap();
            send.finish().unwrap();
        });

        let (mut send, mut recv) = client.open_bi().await.unwrap();
        send.write_all(b"hello").await.unwrap();
        send.finish().unwrap();
        let reply = recv.read_to_end(8).await.unwrap();
        assert_eq!(reply, vec![5]); // server saw 5 bytes

        server_task.await.unwrap();
        pair.close().await;
    }
}
