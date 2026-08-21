//! UDP transport: one datagram == one packet.

use std::io;
use std::time::Duration;

use openay_protocol::{decode, encode, Packet};
use tokio::net::UdpSocket;

use crate::stats::PacketStats;
use crate::SeqTracker;

/// Maximum datagram buffer size: 6 bytes header + 65535 bytes payload.
const BUF_SIZE: usize = 65541;

/// Listens on a UDP socket and decodes incoming datagrams into packets.
///
/// Malformed datagrams (those that fail [`decode`]) are counted in `.stats()`
/// and silently dropped — the receiver loop continues.
pub struct UdpReceiver {
    socket: UdpSocket,
    stats: PacketStats,
    buf: Box<[u8; BUF_SIZE]>,
    seq_tracker: SeqTracker,
}

impl UdpReceiver {
    /// Bind to `127.0.0.1:{port}`. Pass `port = 0` to let the OS assign one.
    pub async fn bind(port: u16) -> io::Result<Self> {
        let addr = format!("127.0.0.1:{port}");
        let socket = UdpSocket::bind(&addr).await?;
        Ok(UdpReceiver {
            socket,
            stats: PacketStats::default(),
            buf: Box::new([0u8; BUF_SIZE]),
            seq_tracker: SeqTracker::new(),
        })
    }

    /// Wait up to `timeout` for the next valid packet.
    ///
    /// Returns `Ok(Some(packet))` on success. Malformed datagrams are silently
    /// counted via `.stats_mut()` and the timeout is *not* reset (a steady
    /// stream of garbage will delay the `None` return).
    ///
    /// Returns `Ok(None)` if no *valid* packet arrived within the deadline.
    /// Returns `Err` on an I/O error from the socket itself.
    pub async fn recv_packet(&mut self, timeout: Duration) -> io::Result<Option<Packet>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            match tokio::time::timeout(deadline - now, self.socket.recv_from(&mut *self.buf)).await
            {
                Err(_elapsed) => return Ok(None),
                Ok(Err(e)) => return Err(e),
                Ok(Ok((n, _src))) => {
                    match decode(&self.buf[..n]) {
                        Ok(pkt) => {
                            self.stats.received += 1;
                            match self.seq_tracker.update(pkt.seq) {
                                crate::SeqEvent::Gap(lost) => self.stats.lost += lost as u64,
                                crate::SeqEvent::Duplicate => self.stats.duplicate += 1,
                                crate::SeqEvent::Reorder => self.stats.out_of_order += 1,
                                _ => {}
                            }
                            return Ok(Some(pkt));
                        }
                        Err(_) => {
                            self.stats.malformed += 1;
                            // loop — keep waiting; the deadline is still counting.
                        }
                    }
                }
            }
        }
    }

    /// Borrow the cumulative receive statistics.
    pub fn stats(&self) -> &PacketStats {
        &self.stats
    }

    /// Mutably borrow the stats (for the loopback CLI to increment
    /// `content_errors`).
    pub fn stats_mut(&mut self) -> &mut PacketStats {
        &mut self.stats
    }

    /// The local port this receiver is bound to.
    pub fn local_port(&self) -> io::Result<u16> {
        self.socket.local_addr().map(|a| a.port())
    }

    /// Request a larger kernel receive buffer (`SO_RCVBUF`). Useful on Wi-Fi
    /// where bursts of audio datagrams can otherwise overflow the socket
    /// queue; the kernel may cap the request at `net.core.rmem_max`.
    pub fn set_recv_buffer_size(&self, size: usize) -> io::Result<()> {
        // tokio's UdpSocket does not expose buffer-size setters; route the
        // setsockopt through socket2's SockRef (same fd, zero-copy).
        socket2::SockRef::from(&self.socket).set_recv_buffer_size(size)
    }

    /// Consume the receiver and return the underlying socket and stats.
    pub fn into_inner(self) -> (UdpSocket, PacketStats) {
        (self.socket, self.stats)
    }
}

/// Sends packets over a connected UDP socket (one datagram per packet).
pub struct UdpSender {
    socket: UdpSocket,
}

impl UdpSender {
    /// Connect to `host:port`. The socket is bound to a local ephemeral port.
    pub async fn connect(host: &str, port: u16) -> io::Result<Self> {
        // Bind first so we have a known local address.
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        socket.connect(format!("{host}:{port}")).await?;
        Ok(UdpSender { socket })
    }

    /// Send one packet as a single datagram.
    pub async fn send_packet(&self, packet: &Packet) -> io::Result<usize> {
        let wire = encode(packet);
        self.socket.send(&wire).await
    }

    /// Consume the sender and return the underlying socket.
    pub fn into_inner(self) -> UdpSocket {
        self.socket
    }
}
