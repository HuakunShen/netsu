//! iperf3 session cookies (`make_cookie` in `iperf_util.c`).
//!
//! A cookie is 36 random characters drawn from a 32-character alphabet,
//! followed by a NUL terminator, for a 37-byte wire form (`COOKIE_SIZE`).
//! The alphabet's length is a power of two so `byte % 32` has no modulo
//! bias — every alphabet character is equally likely.

use rand::Rng;

use super::states::COOKIE_SIZE;

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// 36 random chars from iperf3's cookie alphabet.
pub fn make_cookie() -> String {
    let mut rng = rand::thread_rng();
    (0..COOKIE_SIZE - 1)
        .map(|_| {
            let byte: u8 = rng.r#gen();
            ALPHABET[(byte % ALPHABET.len() as u8) as usize] as char
        })
        .collect()
}

/// Wire form: 36 ASCII chars + NUL = 37 bytes.
pub fn cookie_to_bytes(c: &str) -> [u8; COOKIE_SIZE] {
    let mut out = [0u8; COOKIE_SIZE];
    let bytes = c.as_bytes();
    let n = bytes.len().min(COOKIE_SIZE - 1);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Reads a NUL-terminated cookie back out of its wire form.
pub fn bytes_to_cookie(b: &[u8]) -> String {
    let end = b.iter().position(|&byte| byte == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_36_char_cookies_from_the_iperf3_alphabet() {
        let c = make_cookie();
        assert_eq!(c.len(), 36);
        assert!(
            c.chars()
                .all(|ch| "abcdefghijklmnopqrstuvwxyz234567".contains(ch))
        );
        assert_ne!(make_cookie(), c);
    }

    #[test]
    fn round_trips_through_37_byte_nul_terminated_wire_form() {
        let c = make_cookie();
        let b = cookie_to_bytes(&c);
        assert_eq!(b.len(), COOKIE_SIZE);
        assert_eq!(b[36], 0);
        assert_eq!(bytes_to_cookie(&b), c);
    }
}
