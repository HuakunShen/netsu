//! Protocol core: state constants, cookie handling, the transport-agnostic
//! byte-pipe abstraction, and message framing.
//!
//! Everything under this module is pure logic — no `tokio::net`, no
//! `std::net`, no socket types of any kind. Transports (TCP, UDP, WebSocket)
//! live above this layer and are reached only through the [`pipe::BytePipe`]
//! trait, which is what keeps this code unit-testable without sockets.

pub mod cookie;
pub mod framing;
pub mod params;
pub mod pipe;
pub mod results;
pub mod states;
