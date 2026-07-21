//! Resolving a `--peer` argument into a dialable ticket string. The argument is
//! either a rendez-key short code (~8 chars) or a full `EndpointTicket`
//! (hundreds of chars); they are told apart purely by length.
//!
//! This module also carries the *rendezvous blob* helpers the TUI uses to make
//! one short code work for every transport, not just iroh: the blob a server
//! publishes is `TAG|ADDR` (e.g. `tcp|192.168.1.5:5201` or `iroh|endpointace…`),
//! so a client that claims the code learns both which transport to speak and
//! where to reach the peer — no separate transport pick, no long ticket to type.

use std::net::IpAddr;

use crate::p2p::rendezkey;

/// A rendez-key code is ~8 chars (optionally `XXXX-XXXX`); an `EndpointTicket`
/// is hundreds. 16 is comfortably between the two populations.
const MAX_CODE_LEN: usize = 16;

/// True if `arg` looks like a rendez-key short code rather than a full ticket.
/// Hyphens and whitespace (the code's display separators) are ignored.
pub fn looks_like_code(arg: &str) -> bool {
    let significant = arg
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .count();
    significant > 0 && significant <= MAX_CODE_LEN
}

/// Resolve `arg` into a ticket string: claim it via rendez-key if it looks like
/// a short code, otherwise pass it through as a literal ticket.
pub async fn resolve_ticket(arg: &str, base_url: &str) -> anyhow::Result<String> {
    if looks_like_code(arg) {
        rendezkey::claim(base_url, arg).await
    } else {
        Ok(arg.trim().to_string())
    }
}

/// Build the rendezvous blob a server publishes under its rendez-key code:
/// `TAG|ADDR`. `tag` is the transport (`tcp`/`udp`/`ws`/`iroh`); `addr` is
/// `host:port` for the socket transports or an iroh ticket for `iroh`.
pub fn encode_rendezvous(tag: &str, addr: &str) -> String {
    format!("{}|{}", tag.trim(), addr.trim())
}

/// Split a claimed rendezvous blob into `(tag, addr)`. A blob with no `|` is a
/// bare iroh ticket — the historical format `netsu server --iroh` still stores —
/// so codes minted by older servers keep resolving to `("iroh", ticket)`.
pub fn decode_rendezvous(blob: &str) -> (String, String) {
    match blob.split_once('|') {
        Some((tag, addr)) => (tag.trim().to_string(), addr.trim().to_string()),
        None => ("iroh".to_string(), blob.trim().to_string()),
    }
}

/// Best-effort primary outbound IPv4 address, for advertising a tcp/udp/ws
/// server to a peer on the same network. Uses the connect-trick: a *connected*
/// UDP socket adopts the route's source address without sending a packet, which
/// is the dependency-free way to learn "which local IP would reach the outside
/// world". Returns `None` when the only route is loopback/unspecified (e.g. no
/// network) — the caller then falls back to prompting for the host.
///
/// Caveat worth surfacing in the UI: on a host with a VPN/tunnel default route
/// (e.g. a TUN interface using fake 198.18.x.x addresses), this returns the
/// tunnel's source IP, which a LAN peer cannot reach. That is why the TUI shows
/// the detected address and lets the user edit it before publishing the code.
pub fn local_ipv4() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    // A UDP `connect` only fixes the peer for routing; nothing is transmitted,
    // so the target address need not be reachable — it just selects the route.
    sock.connect(("8.8.8.8", 80)).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    match ip {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(ip),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_codes_are_detected_as_codes() {
        assert!(looks_like_code("7K3M-Q9TX"));
        assert!(looks_like_code("7k3mq9tx"));
        assert!(looks_like_code("7K3M Q9TX"));
        assert!(looks_like_code("ABCD1234"));
    }

    #[test]
    fn full_tickets_are_not_codes() {
        // A real EndpointTicket is ~90+ chars.
        let ticket = "endpointacevg5tl3tlrry6g2ubwlcitdhfgv3xtry6z6yuapfsnapvlitlhkaqbadakqbqk7kiqgaiaycuiwa72sebq";
        assert!(!looks_like_code(ticket));
        assert!(ticket.len() > MAX_CODE_LEN);
    }

    #[test]
    fn empty_is_not_a_code() {
        assert!(!looks_like_code(""));
        assert!(!looks_like_code("   "));
    }

    #[test]
    fn boundary_length_is_a_code() {
        // Exactly 16 significant chars → still a code; 17 → a ticket.
        assert!(looks_like_code("0123456789abcdef")); // 16
        assert!(!looks_like_code("0123456789abcdef0")); // 17
    }

    #[test]
    fn rendezvous_blob_round_trips_per_transport() {
        for (tag, addr) in [
            ("tcp", "192.168.1.5:5201"),
            ("udp", "10.0.0.9:5201"),
            ("ws", "192.168.1.5:5201"),
        ] {
            let blob = encode_rendezvous(tag, addr);
            assert_eq!(
                decode_rendezvous(&blob),
                (tag.to_string(), addr.to_string())
            );
        }
    }

    #[test]
    fn iroh_blob_carries_the_ticket_verbatim() {
        // Tickets are base32 (no '|'), so encode/decode is lossless.
        let ticket = "endpointacevg5tl3tlrry6g2ubwlcitdhfgv3xtry6z6yuapfsnapvlitlhkaqbadakqbqk";
        let blob = encode_rendezvous("iroh", ticket);
        assert_eq!(
            decode_rendezvous(&blob),
            ("iroh".to_string(), ticket.to_string())
        );
    }

    #[test]
    fn bare_ticket_without_a_tag_decodes_as_iroh() {
        // A code minted by an older `netsu server --iroh` stores just the ticket.
        let ticket = "endpointacevg5tl3tlrry6g2ubwlcitdhfgv3xtry6z6yuapfsnapvlitlhkaqbadakqbqk";
        assert_eq!(
            decode_rendezvous(ticket),
            ("iroh".to_string(), ticket.to_string())
        );
    }
}
