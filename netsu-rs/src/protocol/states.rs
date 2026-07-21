//! iperf3 control-channel states (`iperf_api.h`).
//!
//! These are written to the wire as a single **signed** byte — see
//! `PROTOCOL.md`'s "State bytes" section. `ACCESS_DENIED` and `SERVER_ERROR`
//! are negative, which is why every constant here is `i8` rather than `u8`.

pub const TEST_START: i8 = 1;
pub const TEST_RUNNING: i8 = 2;
pub const TEST_END: i8 = 4;
pub const PARAM_EXCHANGE: i8 = 9;
pub const CREATE_STREAMS: i8 = 10;
pub const SERVER_TERMINATE: i8 = 11;
pub const CLIENT_TERMINATE: i8 = 12;
pub const EXCHANGE_RESULTS: i8 = 13;
pub const DISPLAY_RESULTS: i8 = 14;
pub const IPERF_START: i8 = 15;
pub const IPERF_DONE: i8 = 16;
pub const ACCESS_DENIED: i8 = -1;
pub const SERVER_ERROR: i8 = -2;

/// 36 random cookie chars + 1 NUL terminator, per `PROTOCOL.md`'s "Cookie"
/// section.
pub const COOKIE_SIZE: usize = 37;
