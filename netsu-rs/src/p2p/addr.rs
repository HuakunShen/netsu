//! Resolving a `--peer` argument into a dialable ticket string. The argument is
//! either a rendez-key short code (~8 chars) or a full `EndpointTicket`
//! (hundreds of chars); they are told apart purely by length.

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
}
