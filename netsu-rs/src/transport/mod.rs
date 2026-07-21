//! Concrete network transports implementing `protocol::pipe::BytePipe` for
//! the control channel and `streams::channel::DataChannel` for the bulk
//! payload channel. UDP is packet-based and does not use those traits — see
//! [`udp`].

#[cfg(feature = "iroh")]
pub mod iroh;
pub mod tcp;
pub mod udp;
#[cfg(feature = "ws")]
pub mod ws;
