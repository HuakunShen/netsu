//! Bulk data-channel abstractions for the post-handshake payload transfer.
//! Distinct from `protocol::pipe::BytePipe`, which is the control channel's
//! framed, pull-based byte stream: a [`channel::DataChannel`] moves opaque
//! chunks and carries no framing of its own.

pub mod channel;
pub mod runner;
