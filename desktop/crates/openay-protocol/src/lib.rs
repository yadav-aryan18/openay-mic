//! OpenAY Mic wire protocol (v1).
//!
//! Canonical spec: `shared/protocol.md` at the repository root. The packet
//! layout is a 6-byte big-endian header followed by the payload:
//!
//! | Offset | Size | Field          |
//! |--------|------|----------------|
//! | 0      | 1    | magic `0xA7`   |
//! | 1      | 1    | type           |
//! | 2      | 2    | sequence       |
//! | 4      | 2    | payload length |
//!
//! Golden vectors live in `shared/test-vectors.json` and are exercised in
//! the `tests` module of this crate.

/// First header byte of every packet. Receivers drop datagrams that do not
/// start with this byte.
pub const MAGIC: u8 = 0xA7;

/// Header size in bytes: magic + type + 2-byte seq + 2-byte payload length.
pub const HEADER_LEN: usize = 6;

/// Payload type tag (header byte 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PayloadType {
    /// Raw PCM: signed 16-bit little-endian, mono, 48 kHz.
    Pcm = 0,
    /// One Opus packet, mono, 48 kHz.
    Opus = 1,
    /// UTF-8 JSON control message (handshake/stats/bye).
    Control = 2,
}

impl TryFrom<u8> for PayloadType {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(PayloadType::Pcm),
            1 => Ok(PayloadType::Opus),
            2 => Ok(PayloadType::Control),
            _ => Err(DecodeError::ReservedType),
        }
    }
}

impl From<PayloadType> for u8 {
    fn from(t: PayloadType) -> u8 {
        t as u8
    }
}

/// A decoded (or to-be-encoded) wire packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub kind: PayloadType,
    pub seq: u16,
    pub payload: Vec<u8>,
}

/// Errors produced by [`decode`] and [`payload_len_from_header`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// First byte is not [`MAGIC`].
    BadMagic,
    /// Fewer than 6 bytes, fewer than `payload_len` payload bytes, or (for
    /// [`decode`], which demands an exact datagram) trailing bytes after the
    /// declared payload.
    Truncated,
    /// Header type byte is not one of {0, 1, 2}.
    ReservedType,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadMagic => write!(f, "bad magic byte (expected 0xA7)"),
            DecodeError::Truncated => write!(f, "truncated packet (header or payload)"),
            DecodeError::ReservedType => write!(f, "reserved packet type"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encode a packet into its wire representation (6-byte header + payload).
pub fn encode(packet: &Packet) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + packet.payload.len());
    out.push(MAGIC);
    out.push(packet.kind as u8);
    out.extend_from_slice(&packet.seq.to_be_bytes());
    out.extend_from_slice(&(packet.payload.len() as u16).to_be_bytes());
    out.extend_from_slice(&packet.payload);
    out
}

/// Decode one complete datagram (one datagram == one packet).
///
/// The input length must equal `6 + payload_len` exactly: fewer bytes than
/// the declared payload is [`DecodeError::Truncated`], and trailing bytes
/// after the declared payload are also rejected as [`DecodeError::Truncated`]
/// (a datagram that is not byte-exact is not a well-formed packet).
pub fn decode(buf: &[u8]) -> Result<Packet, DecodeError> {
    if buf.len() < HEADER_LEN {
        return Err(DecodeError::Truncated);
    }
    if buf[0] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let kind = PayloadType::try_from(buf[1])?;
    let seq = u16::from_be_bytes([buf[2], buf[3]]);
    let payload_len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    if buf.len() != HEADER_LEN + payload_len {
        return Err(DecodeError::Truncated);
    }
    Ok(Packet {
        kind,
        seq,
        payload: buf[HEADER_LEN..].to_vec(),
    })
}

/// Extract the payload length from a 6-byte header (used by TCP stream
/// framing to know how many payload bytes follow).
///
/// Validates magic and type; rejects a reserved type even though the length
/// field itself is still readable, so a corrupt stream is caught early.
pub fn payload_len_from_header(header: &[u8; HEADER_LEN]) -> Result<u16, DecodeError> {
    if header[0] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    PayloadType::try_from(header[1])?;
    Ok(u16::from_be_bytes([header[4], header[5]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct VectorsFile {
        vectors: Vec<Vector>,
    }

    #[derive(Deserialize)]
    struct Vector {
        name: String,
        hex: String,
        #[serde(default)]
        expect: Option<Expect>,
        #[serde(default)]
        error: Option<String>,
    }

    #[derive(Deserialize, Debug)]
    struct Expect {
        #[serde(rename = "type")]
        kind: String,
        seq: u16,
        payload_hex: String,
    }

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        assert!(hex.len().is_multiple_of(2), "bad hex length: {hex}");
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn kind_from_name(name: &str) -> PayloadType {
        match name {
            "pcm" => PayloadType::Pcm,
            "opus" => PayloadType::Opus,
            "control" => PayloadType::Control,
            other => panic!("unknown type name {other}"),
        }
    }

    fn error_from_name(name: &str) -> DecodeError {
        match name {
            "bad_magic" => DecodeError::BadMagic,
            "truncated" => DecodeError::Truncated,
            "reserved_type" => DecodeError::ReservedType,
            other => panic!("unknown error name {other}"),
        }
    }

    /// The spec's literal `../../shared` would resolve to
    /// `desktop/shared` from this crate (3 levels below the repo root); the
    /// vectors actually live at the repo root, so we ascend three levels.
    const VECTORS_PATH: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../shared/test-vectors.json");

    fn load_vectors() -> VectorsFile {
        let text = std::fs::read_to_string(VECTORS_PATH)
            .expect("shared/test-vectors.json must exist at repo root");
        serde_json::from_str(&text).expect("test-vectors.json must be valid JSON")
    }

    #[test]
    fn golden_vectors() {
        let file = load_vectors();
        assert!(!file.vectors.is_empty(), "no vectors loaded");
        for v in &file.vectors {
            let wire = hex_to_bytes(&v.hex);
            match (&v.expect, &v.error) {
                (Some(exp), None) => {
                    let expected_packet = Packet {
                        kind: kind_from_name(&exp.kind),
                        seq: exp.seq,
                        payload: hex_to_bytes(&exp.payload_hex),
                    };
                    // decode(wire) == expected fields
                    let decoded = decode(&wire).unwrap_or_else(|e| {
                        panic!("vector {}: decode failed: {e}", v.name)
                    });
                    assert_eq!(decoded, expected_packet, "vector {}: decode mismatch", v.name);
                    // encode(expected) == wire
                    let encoded = encode(&expected_packet);
                    assert_eq!(encoded, wire, "vector {}: encode mismatch", v.name);
                }
                (None, Some(err)) => {
                    let expected_err = error_from_name(err);
                    let got = decode(&wire);
                    assert!(
                        matches!(got, Err(e) if e == expected_err),
                        "vector {}: expected error {expected_err:?}, got {got:?}",
                        v.name
                    );
                }
                other => panic!("vector {}: must have exactly one of expect/error: {other:?}", v.name),
            }
        }
    }

    #[test]
    fn payload_len_from_header_matches_vectors() {
        let file = load_vectors();
        for v in &file.vectors {
            let bytes = hex_to_bytes(&v.hex);
            // Some vectors (truncated_header) are shorter than 6 bytes and
            // cannot produce a full header for payload_len_from_header.
            if bytes.len() < HEADER_LEN {
                continue;
            }
            let header: [u8; HEADER_LEN] = bytes[..HEADER_LEN].try_into().unwrap();
            let res = payload_len_from_header(&header);
            match (&v.expect, &v.error) {
                (Some(exp), None) => {
                    let want = hex_to_bytes(&exp.payload_hex).len() as u16;
                    assert_eq!(res, Ok(want), "vector {}: payload_len mismatch", v.name);
                }
                (None, Some(err)) => {
                    match error_from_name(err) {
                        // Header-level errors are visible to the framing helper.
                        DecodeError::BadMagic | DecodeError::ReservedType => {
                            assert_eq!(
                                res,
                                Err(error_from_name(err)),
                                "vector {}: payload_len error mismatch",
                                v.name
                            );
                        }
                        // `truncated` with a well-formed header (e.g.
                        // truncated_payload) is only detectable after reading
                        // the payload; the header itself still parses.
                        DecodeError::Truncated => {
                            let want = u16::from_be_bytes([header[4], header[5]]);
                            assert_eq!(
                                res,
                                Ok(want),
                                "vector {}: payload_len mismatch (truncated vector with valid header)",
                                v.name
                            );
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn roundtrip_payload_sizes_and_seq_wraparound() {
        let sizes = [0usize, 1, 255, 256, 960, 1400, 65535];
        let seqs = [
            0u16, 1, 2, 0x7FFF, 0x8000, 0xFFFE, 0xFFFF,
        ];
        for size in sizes {
            for seq in seqs {
                let payload: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(31)).collect();
                let packet = Packet {
                    kind: PayloadType::Pcm,
                    seq,
                    payload: payload.clone(),
                };
                let wire = encode(&packet);
                assert_eq!(wire.len(), HEADER_LEN + size);
                assert_eq!(wire[0], MAGIC);
                assert_eq!(decode(&wire), Ok(packet), "roundtrip size={size} seq={seq}");
            }
        }
        // Explicit wraparound continuation: 0xFFFE -> 0xFFFF -> 0x0000 -> 0x0001.
        let run = [0xFFFEu16, 0xFFFF, 0x0000, 0x0001];
        let mut expected = 0xFFFEu16;
        for seq in run {
            assert_eq!(expected, seq, "test setup");
            let wire = encode(&Packet {
                kind: PayloadType::Opus,
                seq,
                payload: vec![0xAB; 10],
            });
            assert_eq!(decode(&wire).unwrap().seq, seq);
            expected = expected.wrapping_add(1);
        }
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        // A valid packet with one stray trailing byte must be rejected.
        let packet = Packet {
            kind: PayloadType::Pcm,
            seq: 7,
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        let mut wire = encode(&packet);
        wire.push(0x00);
        assert_eq!(decode(&wire), Err(DecodeError::Truncated));
        // Short payload must be rejected as truncated.
        wire.pop();
        wire.pop();
        assert_eq!(decode(&wire), Err(DecodeError::Truncated));
    }

    #[test]
    fn try_from_payload_type() {
        assert_eq!(PayloadType::try_from(0), Ok(PayloadType::Pcm));
        assert_eq!(PayloadType::try_from(1), Ok(PayloadType::Opus));
        assert_eq!(PayloadType::try_from(2), Ok(PayloadType::Control));
        assert_eq!(PayloadType::try_from(3), Err(DecodeError::ReservedType));
        assert_eq!(PayloadType::try_from(0xFF), Err(DecodeError::ReservedType));
    }
}
