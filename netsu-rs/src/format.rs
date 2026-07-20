//! CLI value parsing and human-readable output formatting. Kept separate from
//! `main.rs` so the parsing is unit-testable without spawning the binary, and
//! matched to `packages/netsu/src/format.ts` so the two implementations' `-b`,
//! `-l`, and interval-line output are drop-in comparable.

use crate::error::{NetsuError, Result};
use crate::stats::IntervalReport;

/// Splits an optional single-letter K/M/G suffix off `s` and returns the
/// numeric prefix scaled by the matching multiplier. `kilo`/`mega`/`giga` let
/// the caller choose decimal (1000) or binary (1024) bases — iperf3 uses
/// different bases for `-b` (decimal) and `-l` (binary), deliberately.
fn parse_suffixed(s: &str, kilo: f64, mega: f64, giga: f64, what: &str) -> Result<f64> {
    let invalid = || NetsuError::Protocol(format!("invalid {what}: {s}"));
    let (num_str, mult) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], kilo),
        Some('m' | 'M') => (&s[..s.len() - 1], mega),
        Some('g' | 'G') => (&s[..s.len() - 1], giga),
        Some(_) => (s, 1.0),
        None => return Err(invalid()),
    };
    let num: f64 = num_str.parse().map_err(|_| invalid())?;
    if num < 0.0 || !num.is_finite() {
        return Err(invalid());
    }
    Ok(num * mult)
}

/// `"5M"` -> 5_000_000 bits/s. K/M/G are **decimal**, like iperf3's `-b`
/// (verified: `iperf3 -b 1M` reports exactly 1.00 Mbits/sec). `"0"` is iperf3's
/// "unlimited".
pub fn parse_bandwidth(s: &str) -> Result<u64> {
    let v = parse_suffixed(s, 1e3, 1e6, 1e9, "bandwidth")?;
    Ok(v.round() as u64)
}

/// `"128K"` -> 131072 bytes. Unlike `-b`, iperf3's `-l` block-size suffixes are
/// **1024-based**. Must be at least 1 byte.
pub fn parse_len(s: &str) -> Result<usize> {
    let v = parse_suffixed(s, 1024.0, 1024.0 * 1024.0, 1024.0 * 1024.0 * 1024.0, "len")?;
    let bytes = v.round() as usize;
    if bytes < 1 {
        return Err(NetsuError::Protocol(format!("invalid len: {s}")));
    }
    Ok(bytes)
}

/// iperf3-style rounding: integers and values >= 100 print whole, else 2 dp.
fn fmt_value(value: f64) -> String {
    if value >= 100.0 || value.fract() == 0.0 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.2}")
    }
}

/// `1_500_000` -> `"1.43 MBytes"` (1024-based, matching iperf3's byte columns).
pub fn format_bytes(n: u64) -> String {
    let units = ["Bytes", "KBytes", "MBytes", "GBytes", "TBytes"];
    let mut value = n as f64;
    let mut i = 0;
    while value >= 1024.0 && i < units.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    format!("{} {}", fmt_value(value), units[i])
}

/// `1_500_000` -> `"1.50 Mbits/sec"` (1000-based, matching iperf3's rate columns).
pub fn format_bits(n: f64) -> String {
    let units = ["bits/sec", "Kbits/sec", "Mbits/sec", "Gbits/sec"];
    let mut value = n;
    let mut i = 0;
    while value >= 1000.0 && i < units.len() - 1 {
        value /= 1000.0;
        i += 1;
    }
    format!("{} {}", fmt_value(value), units[i])
}

/// One periodic report line, matching iperf3's `[SUM]` interval rows.
pub fn interval_line(r: &IntervalReport) -> String {
    let range = format!("{:.2}-{:.2}", r.start, r.end);
    format!(
        "[SUM] {:>11} sec  {:>12}  {:>14}",
        range,
        format_bytes(r.bytes),
        format_bits(r.bits_per_second)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_suffixes_are_decimal_matching_iperf3() {
        assert_eq!(parse_bandwidth("1000").unwrap(), 1_000);
        assert_eq!(parse_bandwidth("10K").unwrap(), 10_000);
        assert_eq!(parse_bandwidth("1M").unwrap(), 1_000_000);
        assert_eq!(parse_bandwidth("1G").unwrap(), 1_000_000_000);
        assert_eq!(parse_bandwidth("0").unwrap(), 0); // iperf3's "unlimited"
        assert!(parse_bandwidth("fast").is_err());
    }

    #[test]
    fn len_suffixes_are_binary_because_it_is_a_byte_count() {
        assert_eq!(parse_len("1460").unwrap(), 1460);
        assert_eq!(parse_len("128K").unwrap(), 131_072);
        assert_eq!(parse_len("1M").unwrap(), 1_048_576);
        assert!(parse_len("big").is_err());
    }
}
