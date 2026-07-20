//! PARAM_EXCHANGE JSON codec (client → server), field names from iperf3's
//! `send_parameters()` — see `PROTOCOL.md`'s "PARAM_EXCHANGE JSON" section.

use serde::Deserialize;

use crate::error::{NetsuError, Result};

pub const DEFAULT_TCP_LEN: usize = 131072;
pub const DEFAULT_UDP_LEN: usize = 1460;
pub const DEFAULT_UDP_BANDWIDTH: u64 = 1048576; // 1 Mbit/s, iperf3's UDP default
pub const MAX_PARALLEL: u32 = 128;
pub const MAX_LEN: usize = 1048576;
pub const MAX_TIME: u32 = 86400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestParams {
    pub udp: bool,
    pub time: u32,
    pub parallel: u32,
    pub len: usize,
    pub reverse: bool,
    pub bandwidth: u64, // bits/s; 0 = unpaced (TCP)
}

/// PARAM_EXCHANGE payload, field names from iperf3's `send_parameters()`.
pub fn encode(p: &TestParams) -> serde_json::Value {
    let mut msg = serde_json::json!({
        "omit": 0,
        "time": p.time,
        "num": 0,
        "blockcount": 0,
        "parallel": p.parallel,
        "len": p.len,
        "pacing_timer": 1000,
        "client_version": format!("netsu-rs-{}", crate::VERSION),
    });

    // Built as an object literal above, so this always matches.
    if let serde_json::Value::Object(map) = &mut msg {
        let proto = if p.udp { "udp" } else { "tcp" };
        map.insert(proto.to_string(), serde_json::json!(true));
        if p.reverse {
            map.insert("reverse".to_string(), serde_json::json!(true));
        }
        if p.udp {
            map.insert("bandwidth".to_string(), serde_json::json!(p.bandwidth));
        }
    }

    msg
}

/// Wire shape for incoming PARAM_EXCHANGE JSON. `#[serde(default)]` on the
/// optional fields lets a missing key fall back to its default (`false`/`0`)
/// rather than erroring — unknown fields (iperf3 sends many more) are
/// tolerated by serde's ordinary struct behavior, since we don't set
/// `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Deserialize)]
struct WireParams {
    #[serde(default)]
    tcp: bool,
    #[serde(default)]
    udp: bool,
    time: u32,
    parallel: u32,
    #[serde(default)]
    reverse: bool,
    len: usize,
    #[serde(default)]
    bandwidth: u64,
}

/// Decodes and validates a PARAM_EXCHANGE payload. Rejects both `tcp` and
/// `udp` present, neither present, and any of the three bounded fields
/// (`parallel`, `len`, `time`) outside their wire-sanity range — see
/// `PROTOCOL.md`'s "Accepted bounds" table.
pub fn decode(v: serde_json::Value) -> Result<TestParams> {
    let w: WireParams = serde_json::from_value(v)?;

    if !w.tcp && !w.udp {
        return Err(NetsuError::Protocol(
            "params: neither tcp nor udp present".into(),
        ));
    }
    if w.tcp && w.udp {
        return Err(NetsuError::Protocol(
            "params: both tcp and udp present".into(),
        ));
    }
    if !(1..=MAX_PARALLEL).contains(&w.parallel) {
        return Err(NetsuError::Protocol(format!(
            "params: parallel out of bounds (1..={MAX_PARALLEL}): {}",
            w.parallel
        )));
    }
    if !(4..=MAX_LEN).contains(&w.len) {
        return Err(NetsuError::Protocol(format!(
            "params: len out of bounds (4..={MAX_LEN}): {}",
            w.len
        )));
    }
    if !(1..=MAX_TIME).contains(&w.time) {
        return Err(NetsuError::Protocol(format!(
            "params: time out of bounds (1..={MAX_TIME}): {}",
            w.time
        )));
    }

    Ok(TestParams {
        udp: w.udp,
        time: w.time,
        parallel: w.parallel,
        len: w.len,
        reverse: w.reverse,
        bandwidth: w.bandwidth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TestParams {
        TestParams {
            udp: false,
            time: 10,
            parallel: 2,
            len: 131072,
            reverse: true,
            bandwidth: 0,
        }
    }

    #[test]
    fn encodes_iperf3_field_names() {
        let j = encode(&sample());
        assert_eq!(j["tcp"], serde_json::json!(true));
        assert_eq!(j["time"], serde_json::json!(10));
        assert_eq!(j["parallel"], serde_json::json!(2));
        assert_eq!(j["len"], serde_json::json!(131072));
        assert_eq!(j["reverse"], serde_json::json!(true));
        assert!(j.get("udp").is_none());
        assert!(j.get("bandwidth").is_none()); // tcp: no pacing field
        assert!(j.get("client_version").is_some());
    }

    #[test]
    fn encodes_udp_with_bandwidth_and_omits_reverse_when_false() {
        let p = TestParams {
            udp: true,
            reverse: false,
            bandwidth: 1048576,
            len: 1460,
            ..sample()
        };
        let j = encode(&p);
        assert_eq!(j["udp"], serde_json::json!(true));
        assert_eq!(j["bandwidth"], serde_json::json!(1048576));
        assert_eq!(j["len"], serde_json::json!(1460));
        assert!(j.get("tcp").is_none());
        assert!(j.get("reverse").is_none());
    }

    #[test]
    fn decodes_its_own_output_and_tolerates_unknown_fields() {
        let mut j = encode(&sample());
        j["MSS"] = serde_json::json!(1400);
        j["congestion"] = serde_json::json!("cubic");
        assert_eq!(decode(j).unwrap(), sample());
    }

    #[test]
    fn round_trips_udp_params() {
        let p = TestParams {
            udp: true,
            time: 10,
            parallel: 2,
            len: 1460,
            reverse: false,
            bandwidth: 1048576,
        };
        assert_eq!(decode(encode(&p)).unwrap(), p);
    }

    #[test]
    fn rejects_out_of_bounds_and_ambiguous_values() {
        assert!(
            decode(serde_json::json!({"tcp": true, "time": 10, "parallel": 500, "len": 1000}))
                .is_err()
        );
        assert!(
            decode(serde_json::json!({"tcp": true, "time": 10, "parallel": 1, "len": 99999999}))
                .is_err()
        );
        assert!(decode(serde_json::json!({"time": 10, "parallel": 1, "len": 1000})).is_err()); // neither
        assert!(decode(serde_json::json!({"tcp": true, "udp": true, "time": 10, "parallel": 1, "len": 1000})).is_err()); // both
        assert!(
            decode(serde_json::json!({"tcp": true, "time": 999999, "parallel": 1, "len": 1000}))
                .is_err()
        ); // time
    }
}
