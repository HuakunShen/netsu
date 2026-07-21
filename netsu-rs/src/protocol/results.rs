//! EXCHANGE_RESULTS JSON codec (both directions), field names from iperf3's
//! `send_results()` — see `PROTOCOL.md`'s "EXCHANGE_RESULTS JSON" section.

use serde::Deserialize;

use crate::error::Result;

fn default_retransmits() -> i64 {
    -1
}

/// A single data stream's transfer statistics. Field names already match the
/// wire's snake_case names, so this struct doubles as the wire shape for
/// decoding.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamResult {
    pub id: u32,
    pub bytes: u64,
    #[serde(default = "default_retransmits")]
    pub retransmits: i64,
    #[serde(default)]
    pub jitter: f64, // seconds
    #[serde(default)]
    pub errors: u64, // UDP lost packets
    #[serde(default)]
    pub packets: u64,
    #[serde(default)]
    pub start_time: f64,
    #[serde(default)]
    pub end_time: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EndResults {
    #[serde(default = "default_retransmits")]
    pub sender_has_retransmits: i64,
    pub streams: Vec<StreamResult>,
}

/// EXCHANGE_RESULTS payload, field names from iperf3's `send_results()`.
/// `cpu_util_*` are always 0 — pure Rust here reports no CPU utilization,
/// same rationale as netsu-ts's Node implementation (see `PROTOCOL.md`'s note
/// on `sender_has_retransmits`).
pub fn encode(r: &EndResults) -> serde_json::Value {
    let streams: Vec<serde_json::Value> = r
        .streams
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "bytes": s.bytes,
                "retransmits": s.retransmits,
                "jitter": s.jitter,
                "errors": s.errors,
                "omitted_errors": 0,
                "packets": s.packets,
                "omitted_packets": 0,
                "start_time": s.start_time,
                "end_time": s.end_time,
            })
        })
        .collect();

    serde_json::json!({
        "cpu_util_total": 0.0,
        "cpu_util_user": 0.0,
        "cpu_util_system": 0.0,
        "sender_has_retransmits": r.sender_has_retransmits,
        "streams": streams,
    })
}

/// Decodes an EXCHANGE_RESULTS payload. Unknown fields (`cpu_util_*`,
/// `omitted_errors`, `omitted_packets`, ...) are tolerated by serde's
/// ordinary struct behavior, since we don't set `#[serde(deny_unknown_fields)]`.
pub fn decode(v: serde_json::Value) -> Result<EndResults> {
    Ok(serde_json::from_value(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_iperf3_field_names() {
        let r = EndResults {
            sender_has_retransmits: -1,
            streams: vec![StreamResult {
                id: 1,
                bytes: 5000,
                retransmits: -1,
                jitter: 0.002,
                errors: 3,
                packets: 100,
                start_time: 0.0,
                end_time: 10.01,
            }],
        };
        let j = encode(&r);
        assert_eq!(j["cpu_util_total"], serde_json::json!(0.0));
        assert_eq!(j["sender_has_retransmits"], serde_json::json!(-1));
        let s = &j["streams"][0];
        assert_eq!(s["id"], serde_json::json!(1));
        assert_eq!(s["bytes"], serde_json::json!(5000));
        assert_eq!(s["jitter"], serde_json::json!(0.002));
        assert_eq!(s["errors"], serde_json::json!(3));
        assert_eq!(s["packets"], serde_json::json!(100));
        assert_eq!(s["start_time"], serde_json::json!(0.0));
        assert_eq!(s["end_time"], serde_json::json!(10.01));
        assert_eq!(decode(j).unwrap(), r);
    }
}
