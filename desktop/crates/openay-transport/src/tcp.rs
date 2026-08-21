//! TCP byte-stream framing: back-to-back packets with bad-magic resync.

use std::io;

use openay_protocol::{
    encode, payload_len_from_header, DecodeError, Packet, PayloadType, HEADER_LEN, MAGIC,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum number of bytes scanned looking for a valid magic byte before
/// giving up and returning a hard I/O error.
const MAX_RESYNC_SCAN: usize = 65536;

/// Wraps a `tokio`-compatible byte stream with OpenAY Mic packet framing.
///
/// Packets are written back-to-back via [`send_packet`] and read via
/// [`next_packet`].  A 6-byte header is read first; if the magic byte is
/// wrong the receiver scans forward up to 64 KiB for the next `0xA7` and
/// resumes. If the magic byte is present but the type is reserved, the
/// stream is considered corrupt and a hard `io::Error` is returned.
pub struct TcpPacketStream<T> {
    stream: T,
    /// Buffer of bytes already read from the stream but not yet consumed as
    /// part of a complete packet (e.g. leftover from a resync scan).
    pending: Vec<u8>,
}

impl<T> TcpPacketStream<T> {
    /// Wrap an existing byte stream.
    pub fn new(stream: T) -> Self {
        TcpPacketStream {
            stream,
            pending: Vec::new(),
        }
    }

    /// Consume the wrapper and return the inner stream.
    pub fn into_inner(self) -> T {
        self.stream
    }

    /// Borrow the inner stream.
    pub fn get_ref(&self) -> &T {
        &self.stream
    }

    /// Mutably borrow the inner stream.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.stream
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> TcpPacketStream<T> {
    /// Encode `packet` and write it as a single framed packet to the stream.
    pub async fn send_packet(&mut self, packet: &Packet) -> io::Result<()> {
        let wire = encode(packet);
        self.stream.write_all(&wire).await
    }

    /// Read the next complete packet from the stream.
    ///
    /// If the magic byte is wrong, the stream is scanned forward up to
    /// [`MAX_RESYNC_SCAN`] bytes for the next `0xA7`; if found, parsing
    /// resumes from that byte. If none is found, a hard `io::Error` is
    /// returned.
    pub async fn next_packet(&mut self) -> io::Result<Packet> {
        loop {
            // Ensure we have at least HEADER_LEN bytes.
            self.pull_up_to(HEADER_LEN).await?;
            let header: [u8; HEADER_LEN] = self.pending[..HEADER_LEN].try_into().unwrap();

            if header[0] != MAGIC {
                // Bad magic — consume the 6 bytes and resync.
                self.pending.drain(..HEADER_LEN);
                if !self.resync().await? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "TcpPacketStream: no magic byte within {} bytes",
                            MAX_RESYNC_SCAN
                        ),
                    ));
                }
                continue;
            }

            let plen = match payload_len_from_header(&header) {
                Ok(n) => n as usize,
                Err(DecodeError::BadMagic) => unreachable!(),
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "TcpPacketStream: reserved packet type in stream",
                    ));
                }
            };

            // Consume the header bytes.
            self.pending.drain(..HEADER_LEN);
            // Ensure we have the full payload.
            self.pull_up_to(plen).await?;
            let payload = self.pending.drain(..plen).collect();

            // We already validated the type via payload_len_from_header, so
            // TryFrom cannot fail here.
            let kind = PayloadType::try_from(header[1]).unwrap();
            let seq = u16::from_be_bytes([header[2], header[3]]);

            return Ok(Packet { kind, seq, payload });
        }
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Ensure `self.pending` contains at least `want` bytes by reading from
    /// the underlying stream.
    async fn pull_up_to(&mut self, want: usize) -> io::Result<()> {
        // Use a fixed-size chunk to avoid tiny reads.
        const CHUNK: usize = 4096;
        while self.pending.len() < want {
            let mut chunk = vec![0u8; CHUNK];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                let msg = if self.pending.is_empty() {
                    "connection closed by peer".to_string()
                } else {
                    format!(
                        "truncated: needed {} bytes, had {} before EOF",
                        want - self.pending.len(),
                        self.pending.len()
                    )
                };
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, msg));
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
        Ok(())
    }

    /// Scan forward in the stream for the next `0xA7` magic byte.
    ///
    /// Returns `true` if found (the magic byte is at position 0 of
    /// `self.pending`). Garbage bytes scanned before the magic are
    /// discarded. If EOF is reached, or the scan limit is exceeded, returns
    /// `false`.
    async fn resync(&mut self) -> io::Result<bool> {
        let mut scanned: usize = 0;
        loop {
            if let Some(i) = self.pending.iter().position(|&b| b == MAGIC) {
                scanned += i;
                // The magic byte must lie within the scan window.
                if scanned > MAX_RESYNC_SCAN {
                    return Ok(false);
                }
                self.pending.drain(..i);
                return Ok(true);
            }
            scanned += self.pending.len();
            self.pending.clear();
            if scanned >= MAX_RESYNC_SCAN {
                return Ok(false);
            }
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(false);
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake in-memory reader/writer pair to test the framing logic.
    use tokio::net::TcpListener;
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn send_and_receive_single_packet() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut rx = TcpPacketStream::new(s);
            rx.next_packet().await
        });

        let client = tokio::spawn(async move {
            let s = TcpStream::connect(addr).await.unwrap();
            let mut tx = TcpPacketStream::new(s);
            let pkt = Packet {
                kind: PayloadType::Pcm,
                seq: 42,
                payload: b"hello".to_vec(),
            };
            tx.send_packet(&pkt).await.unwrap();
            // Close the write end so the server knows when to stop.
            tx.get_mut().shutdown().await.unwrap();
            pkt
        });

        let (rx_res, pkt_sent) = tokio::join!(server, client);
        let pkt_sent = pkt_sent.unwrap();
        let pkt_recv = rx_res.unwrap().unwrap();
        assert_eq!(pkt_sent, pkt_recv);
    }

    #[tokio::test]
    async fn resync_on_bad_magic() {
        // Send: garbage, then a valid packet.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut rx = TcpPacketStream::new(s);
            rx.next_packet().await
        });

        let client = tokio::spawn(async move {
            let s = TcpStream::connect(addr).await.unwrap();
            // 7 garbage bytes followed by a valid packet.
            let garbage = [0xFFu8, 0x00, 0xA7, 0xAA, 0xBB, 0xCC, 0xDD];
            let pkt = Packet {
                kind: PayloadType::Opus,
                seq: 100,
                payload: vec![0x01, 0x02],
            };
            let wire = encode(&pkt);
            let mut stream = s;
            stream.write_all(&garbage).await.unwrap();
            stream.write_all(&wire).await.unwrap();
            stream.shutdown().await.unwrap();

            // The first 0xA7 in garbage is at index 2, but it's not a valid
            // header (the next bytes won't form a valid header). The packet
            // after the garbage should be found.
            pkt
        });

        let (rx_res, pkt_sent) = tokio::join!(server, client);
        let pkt_sent = pkt_sent.unwrap();
        let pkt_recv = rx_res.unwrap().unwrap();
        assert_eq!(pkt_sent, pkt_recv);
    }
}
