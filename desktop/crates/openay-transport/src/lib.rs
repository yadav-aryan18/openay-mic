//! OpenAY Mic transports.
//!
//! Provides the three wire transports defined in `shared/protocol.md`:
//!
//! - [`udp`]: one datagram == one packet, malformed datagrams dropped + counted
//! - [`tcp`]: back-to-back packets on a byte stream with 64 KiB bad-magic
//!   resynchronisation
//! - [`rfcomm_server`] (feature `bluetooth`): RFCOMM SPP-style server whose
//!   accepted byte streams are framed exactly like TCP
//!
//! Plus the shared building blocks: [`SeqTracker`] (mod-2^16 sequencing
//! classification), [`PacketStats`] (the canonical `RECV ...` stats line) and
//! [`fill_xorshift`] (the deterministic interop test filler from the spec).

pub mod filler;
#[cfg(feature = "bluetooth")]
pub mod rfcomm_server;
pub mod seq;
pub mod stats;
pub mod tcp;
pub mod udp;

pub use filler::fill_xorshift;
#[cfg(feature = "bluetooth")]
pub use rfcomm_server::RfcommServer;
pub use seq::{SeqEvent, SeqTracker};
pub use stats::PacketStats;
pub use tcp::TcpPacketStream;
pub use udp::{UdpReceiver, UdpSender};
