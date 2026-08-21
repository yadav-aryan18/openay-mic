//! Packet ingest pipeline: decode payloads into `f32` samples, track
//! sequencing, and push into the shared jitter buffer.
//!
//! [`Ingest`] is owned by the network receive task; the only state shared
//! with the audio output path is the [`JitterBuffer`] behind an `Arc`.

use std::sync::Arc;

use openay_codec::OpusCodec;
use openay_jitter::JitterBuffer;
use openay_protocol::PayloadType;
use openay_transport::SeqTracker;

/// Errors produced by [`Ingest::ingest_packet`]. Malformed and undecodable
/// packets are counted, never fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestError {
    /// The payload could not be interpreted for the declared type.
    MalformedPayload(&'static str),
    /// An Opus packet failed to decode.
    OpusDecode,
    /// A packet of the disallowed type arrived (strict `--codec` modes).
    UnexpectedType,
}

/// Pure-ish ingest pipeline (no I/O): sequence classification + payload
/// decode + jitter-buffer push. Unit-testable without any sockets.
pub struct Ingest {
    jitter: Arc<JitterBuffer>,
    seq: SeqTracker,
    /// Opus decoder; `None` if libopus initialization failed (Opus packets
    /// are then counted as malformed rather than crashing the stream).
    decoder: Option<OpusCodec>,
    /// `--codec` mode: `None` = auto (accept both), `Some` = strict.
    only: Option<PayloadType>,
    /// Overrun log throttle: log at most every N overruns.
    overrun_log_gate: u64,

    pub received: u64,
    pub lost: u64,
    pub duplicate: u64,
    pub out_of_order: u64,
    pub malformed: u64,
}

impl Ingest {
    /// Create an ingest pipeline over `jitter`. `only` restricts accepted
    /// payload types (`None` accepts both PCM and Opus).
    pub fn new(jitter: Arc<JitterBuffer>, only: Option<PayloadType>) -> Self {
        let decoder = OpusCodec::new().ok();
        Ingest {
            jitter,
            seq: SeqTracker::new(),
            decoder,
            only,
            overrun_log_gate: 0,
            received: 0,
            lost: 0,
            duplicate: 0,
            out_of_order: 0,
            malformed: 0,
        }
    }

    /// Classify `seq` against the stream expectation and fold the result
    /// into the counters.
    fn track_seq(&mut self, seq: u16) {
        match self.seq.update(seq) {
            openay_transport::SeqEvent::Gap(lost) => self.lost += lost as u64,
            openay_transport::SeqEvent::Duplicate => self.duplicate += 1,
            openay_transport::SeqEvent::Reorder => self.out_of_order += 1,
            openay_transport::SeqEvent::InOrder => {}
        }
    }

    /// Decode a payload into linear `f32` samples in `[-1.0, 1.0)`.
    ///
    /// `PayloadType::Control` yields no samples (control packets share the
    /// sequence space but carry no audio).
    fn decode(&mut self, kind: PayloadType, payload: &[u8]) -> Result<Vec<f32>, IngestError> {
        match kind {
            PayloadType::Control => Ok(Vec::new()),
            PayloadType::Pcm => {
                // Raw s16le mono 48 kHz; the length field disambiguates
                // 5 ms (480 B) vs 10 ms (960 B) frames.
                if !payload.len().is_multiple_of(2) {
                    return Err(IngestError::MalformedPayload(
                        "PCM payload length is not even",
                    ));
                }
                Ok(payload
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                    .collect())
            }
            PayloadType::Opus => {
                let decoder = self
                    .decoder
                    .as_mut()
                    .ok_or(IngestError::MalformedPayload("Opus decoder unavailable"))?;
                let pcm = decoder
                    .decode(payload)
                    .map_err(|_| IngestError::OpusDecode)?;
                Ok(pcm.iter().map(|&s| s as f32 / 32768.0).collect())
            }
        }
    }

    /// Ingest one decoded packet: classify its sequence number, decode the
    /// payload, and push the samples into the jitter buffer.
    ///
    /// A push that fails to fit drops the whole block and counts an overrun
    /// on the jitter buffer (return value is still `Ok` — an overrun is a
    /// counted condition, not an error).
    pub fn ingest_packet(
        &mut self,
        kind: PayloadType,
        seq: u16,
        payload: &[u8],
    ) -> Result<(), IngestError> {
        self.received += 1;
        self.track_seq(seq);

        if let Some(strict) = self.only {
            if kind != strict {
                self.malformed += 1;
                return Err(IngestError::UnexpectedType);
            }
        }

        let samples = match self.decode(kind, payload) {
            Ok(s) => s,
            Err(e) => {
                self.malformed += 1;
                return Err(e);
            }
        };
        if samples.is_empty() {
            return Ok(()); // control packet
        }

        let pushed = self.jitter.push(&samples);
        if pushed == 0 {
            self.overrun_log_gate += 1;
            let overruns = self.jitter.overruns();
            // Throttled log: first overrun, then every 1000th.
            if self.overrun_log_gate == 1 || self.overrun_log_gate.is_multiple_of(1000) {
                eprintln!(
                    "openay-server: jitter buffer overrun, dropped whole block \
                     (total overruns={overruns})"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openay_codec::FRAME_SAMPLES;
    use openay_protocol::{encode, PayloadType};
    use std::f32::consts::PI;

    fn ingest() -> (Ingest, Arc<JitterBuffer>) {
        let jitter = Arc::new(JitterBuffer::new(8192));
        (Ingest::new(jitter.clone(), None), jitter)
    }

    /// `ingest_packet` on the raw payload bytes (as received from the wire).
    fn feed(
        i: &mut Ingest,
        kind: PayloadType,
        seq: u16,
        payload: &[u8],
    ) -> Result<(), IngestError> {
        i.ingest_packet(kind, seq, payload)
    }

    fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn pcm_conversion_known_values() {
        let (mut i, jitter) = ingest();
        // -32768 (LE 00 80) -> -1.0; 0x7FFF (LE FF 7F) -> 32767/32768;
        // 0 (00 00) -> 0.0; 1 (01 00) -> 1/32768; -1 (FF FF) -> -1/32768.
        let payload = pcm_bytes(&[-32768, 32767, 0, 1, -1]);
        feed(&mut i, PayloadType::Pcm, 0, &payload).unwrap();
        let mut out = vec![0.0f32; 5];
        assert_eq!(jitter.pop(&mut out), 5);
        assert_eq!(out[0], -1.0, "negative full scale");
        assert!((out[1] - 32767.0 / 32768.0).abs() < 1e-6, "got {}", out[1]);
        assert_eq!(out[2], 0.0);
        assert!((out[3] - 1.0 / 32768.0).abs() < 1e-7, "got {}", out[3]);
        assert!((out[4] - (-1.0 / 32768.0)).abs() < 1e-7, "got {}", out[4]);
    }

    #[test]
    fn pcm_odd_payload_is_malformed() {
        let (mut i, jitter) = ingest();
        assert!(matches!(
            feed(&mut i, PayloadType::Pcm, 0, &[0x00, 0x01, 0x02]),
            Err(IngestError::MalformedPayload(_))
        ));
        assert_eq!(i.malformed, 1);
        assert_eq!(jitter.available(), 0);
    }

    #[test]
    fn seq_gap_duplicate_reorder_counted() {
        let (mut i, jitter) = ingest();
        let silence = pcm_bytes(&[0i16; 48]);
        for seq in 0..10u16 {
            feed(&mut i, PayloadType::Pcm, seq, &silence).unwrap();
        }
        assert_eq!(i.lost, 0);
        assert_eq!(i.received, 10);

        // Jump 10 -> 14: 4 lost.
        feed(&mut i, PayloadType::Pcm, 14, &silence).unwrap();
        assert_eq!(i.lost, 4);
        // Duplicate of 14.
        feed(&mut i, PayloadType::Pcm, 14, &silence).unwrap();
        assert_eq!(i.duplicate, 1);
        // Late packet 13 (behind expected 16, not the last seen 14).
        feed(&mut i, PayloadType::Pcm, 13, &silence).unwrap();
        assert_eq!(i.out_of_order, 1);
        // In-order continues.
        feed(&mut i, PayloadType::Pcm, 16, &silence).unwrap();
        assert_eq!(i.received, 14);
        // 14 audio packets x 48 samples, all pushed (no overrun).
        assert_eq!(jitter.available(), 14 * 48, "all silence frames ingested");
    }

    #[test]
    fn control_packet_skips_audio_but_counts() {
        let (mut i, jitter) = ingest();
        feed(&mut i, PayloadType::Control, 0, b"{\"bye\":true}").unwrap();
        assert_eq!(i.received, 1);
        assert_eq!(jitter.available(), 0, "no audio from control packets");
        // Sequence space is shared: next audio packet is in-order.
        let silence = pcm_bytes(&[0i16; 48]);
        feed(&mut i, PayloadType::Pcm, 1, &silence).unwrap();
        assert_eq!(i.lost, 0);
        assert_eq!(jitter.available(), 48);
    }

    #[test]
    fn strict_codec_mode_rejects_other_type() {
        let jitter = Arc::new(JitterBuffer::new(8192));
        let mut i = Ingest::new(jitter.clone(), Some(PayloadType::Opus));
        let pcm = pcm_bytes(&[0i16; 48]);
        assert!(matches!(
            feed(&mut i, PayloadType::Pcm, 0, &pcm),
            Err(IngestError::UnexpectedType)
        ));
        assert_eq!(i.malformed, 1);
        assert_eq!(jitter.available(), 0);
    }

    /// Opus roundtrip through the ingest pipeline: encode one 480-sample
    /// sine frame, ingest it, pop it back, and check the signal survived.
    #[test]
    fn opus_roundtrip_via_ingest() {
        let mut codec = OpusCodec::new().expect("libopus available");
        // 440 Hz sine, amplitude 0.9 FS, one 10 ms frame at 48 kHz.
        let pcm: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                (0.9 * 32767.0 * (2.0 * PI as f64 * 440.0 * t).sin()) as i16
            })
            .collect();
        let opus = codec.encode(&pcm).expect("encode");

        let (mut i, jitter) = ingest();
        feed(&mut i, PayloadType::Opus, 0, &opus).unwrap();
        assert_eq!(jitter.available(), FRAME_SAMPLES);

        let mut out = vec![0.0f32; FRAME_SAMPLES];
        assert_eq!(jitter.pop(&mut out), FRAME_SAMPLES);
        assert_eq!(out.len(), FRAME_SAMPLES);

        let rms =
            (out.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / out.len() as f64).sqrt();
        assert!(rms > 0.05, "decoded sine must have audible RMS, got {rms}");
        assert!(rms < 1.0, "sanity: RMS bounded");
    }

    /// Wire-level roundtrip: a packet encoded with `openay_protocol::encode`
    /// decodes and ingests end to end.
    #[test]
    fn wire_packet_roundtrip() {
        let pkt = openay_protocol::Packet {
            kind: PayloadType::Pcm,
            seq: 7,
            payload: pcm_bytes(&[12345, -12345, 0, 32767, -32768]),
        };
        let wire = encode(&pkt);
        let decoded = openay_protocol::decode(&wire).unwrap();
        let (mut i, jitter) = ingest();
        feed(&mut i, decoded.kind, decoded.seq, &decoded.payload).unwrap();
        let mut out = vec![0.0f32; 5];
        jitter.pop(&mut out);
        assert!((out[0] - 12345.0 / 32768.0).abs() < 1e-5);
        assert!((out[1] - (-12345.0 / 32768.0)).abs() < 1e-5);
        assert_eq!(out[4], -1.0);
    }

    #[test]
    fn overrun_increments_counter_and_is_not_fatal() {
        let jitter = Arc::new(JitterBuffer::new(1024));
        let mut i = Ingest::new(jitter.clone(), None);
        // Fill the buffer exactly.
        let big = pcm_bytes(&[1000i16; 1024]);
        feed(&mut i, PayloadType::Pcm, 0, &big).unwrap();
        assert_eq!(jitter.available(), 1024);
        // One more frame does not fit -> dropped whole, overrun counted.
        feed(&mut i, PayloadType::Pcm, 1, &big).unwrap();
        assert_eq!(jitter.overruns(), 1);
        assert_eq!(jitter.available(), 1024, "block dropped whole");
        // Pipeline still healthy.
        feed(&mut i, PayloadType::Pcm, 2, &pcm_bytes(&[0i16; 2])).unwrap();
        assert_eq!(jitter.overruns(), 2);
    }
}
