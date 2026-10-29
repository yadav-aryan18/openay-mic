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

/// Upper bound for the output gain (4.0 ≈ +12 dB). Values outside
/// `0.0..=MAX_GAIN` are clamped; non-finite values fall back to unity.
pub const MAX_GAIN: f32 = 4.0;

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
    /// Output gain applied to decoded samples before the jitter buffer
    /// (1.0 = unity). Applied receiver-side so the phone's capture → encode
    /// chain is untouched by volume changes.
    gain: f32,
    /// Overrun log throttle: log at most every N overruns.
    overrun_log_gate: u64,

    pub received: u64,
    /// Packets that carried audio (PCM/Opus). The depth controller's
    /// "flowing" signal uses this, not `received`: control traffic
    /// (keepalive pongs) must not earn clean-window prebuffer decay while
    /// the sender is paused.
    pub audio_received: u64,
    pub lost: u64,
    pub duplicate: u64,
    pub out_of_order: u64,
    pub malformed: u64,
}

impl Ingest {
    /// Create an ingest pipeline over `jitter`. `only` restricts accepted
    /// payload types (`None` accepts both PCM and Opus); `gain` scales the
    /// decoded samples (clamped to `0.0..=4.0` here so a bad value can never
    /// amplify into clipping beyond the documented range).
    pub fn new(jitter: Arc<JitterBuffer>, only: Option<PayloadType>, gain: f32) -> Self {
        let decoder = OpusCodec::new().ok();
        Ingest {
            jitter,
            seq: SeqTracker::new(),
            decoder,
            only,
            gain: if gain.is_finite() { gain.clamp(0.0, MAX_GAIN) } else { 1.0 },
            overrun_log_gate: 0,
            received: 0,
            audio_received: 0,
            lost: 0,
            duplicate: 0,
            out_of_order: 0,
            malformed: 0,
        }
    }

    /// Classify `seq` against the stream expectation, fold the result into
    /// the counters, and return the event (the caller skips the payload for
    /// [`SeqEvent::Duplicate`] — a re-delivered frame must not play twice).
    fn track_seq(&mut self, seq: u16) -> openay_transport::SeqEvent {
        match self.seq.update(seq) {
            openay_transport::SeqEvent::Gap(lost) => {
                self.lost += lost as u64;
                openay_transport::SeqEvent::Gap(lost)
            }
            openay_transport::SeqEvent::Duplicate => {
                self.duplicate += 1;
                openay_transport::SeqEvent::Duplicate
            }
            openay_transport::SeqEvent::Reorder => {
                self.out_of_order += 1;
                openay_transport::SeqEvent::Reorder
            }
            openay_transport::SeqEvent::InOrder => openay_transport::SeqEvent::InOrder,
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
        if kind != PayloadType::Control {
            self.audio_received += 1;
        }
        let event = self.track_seq(seq);

        // A duplicate (re-delivered frame) is counted but not decoded or
        // pushed again: playing the same 10 ms twice is an audible echo, and
        // the original copy is already buffered (or arriving out of order).
        if event == openay_transport::SeqEvent::Duplicate {
            return Ok(());
        }

        if let Some(strict) = self.only {
            if kind != strict {
                self.malformed += 1;
                return Err(IngestError::UnexpectedType);
            }
        }

        let mut samples = match self.decode(kind, payload) {
            Ok(s) => s,
            Err(e) => {
                self.malformed += 1;
                return Err(e);
            }
        };
        if samples.is_empty() {
            return Ok(()); // control packet
        }
        // Receiver-side gain, applied after decode and before the jitter
        // buffer (clamped to full scale so boosted samples cannot wrap).
        if self.gain != 1.0 {
            for s in &mut samples {
                *s = (*s * self.gain).clamp(-1.0, 1.0);
            }
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
        (Ingest::new(jitter.clone(), None, 1.0), jitter)
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
        // 13 audio packets x 48 samples pushed: the duplicate of 14 is
        // counted but not buffered again (dedup — the frame must not play
        // twice); the late 13 is a first delivery and IS buffered.
        assert_eq!(jitter.available(), 13 * 48, "audio frames ingested, dup deduped");
    }

    #[test]
    fn duplicate_payload_is_not_pushed_twice() {
        let (mut i, jitter) = ingest();
        let silence = pcm_bytes(&[0i16; 48]);
        feed(&mut i, PayloadType::Pcm, 0, &silence).unwrap();
        assert_eq!(jitter.available(), 48);
        // Immediate re-delivery of the same seq (proxy dup rule): counted,
        // but the samples must not land in the buffer a second time.
        feed(&mut i, PayloadType::Pcm, 0, &silence).unwrap();
        assert_eq!(i.duplicate, 1);
        assert_eq!(i.received, 2);
        assert_eq!(jitter.available(), 48, "duplicate frame deduped");
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
        let mut i = Ingest::new(jitter.clone(), Some(PayloadType::Opus), 1.0);
        let pcm = pcm_bytes(&[0i16; 48]);
        assert!(matches!(
            feed(&mut i, PayloadType::Pcm, 0, &pcm),
            Err(IngestError::UnexpectedType)
        ));
        assert_eq!(i.malformed, 1);
        assert_eq!(jitter.available(), 0);
    }

    /// Receiver-side gain: 2.0 doubles sample amplitude; 0.0 fully silences
    /// while keeping the timing (frames still flow through the buffer); an
    /// out-of-range value is clamped so boosted samples cannot exceed ±1.0.
    #[test]
    fn gain_scales_decoded_samples() {
        let (mut i, jitter) = ingest_with_gain(2.0);
        feed(&mut i, PayloadType::Pcm, 0, &pcm_bytes(&[8192, -8192])).unwrap();
        let mut out = vec![0.0f32; 2];
        jitter.pop(&mut out);
        assert!((out[0] - 16384.0 / 32768.0).abs() < 1e-6, "got {}", out[0]);
        assert!((out[1] - (-16384.0 / 32768.0)).abs() < 1e-6, "got {}", out[1]);

        // Gain 0 mutes but preserves the stream (samples pushed as zeros).
        let (mut i, jitter) = ingest_with_gain(0.0);
        feed(&mut i, PayloadType::Pcm, 0, &pcm_bytes(&[12345, -12345])).unwrap();
        let mut out = vec![1.0f32; 2];
        jitter.pop(&mut out);
        assert_eq!(out, [0.0, 0.0]);

        // Clamping: an 8x request acts as 4x, and full-scale input saturates.
        let (mut i, jitter) = ingest_with_gain(8.0);
        feed(&mut i, PayloadType::Pcm, 0, &pcm_bytes(&[32767, 1000])).unwrap();
        let mut out = vec![0.0f32; 2];
        jitter.pop(&mut out);
        assert_eq!(out[0], 1.0, "boosted full-scale clamps to 1.0");
        assert!(
            (out[1] - 4000.0 / 32768.0).abs() < 1e-6,
            "1000 samples * clamped gain 4 = 4000/32768, got {}",
            out[1]
        );

        // Non-finite gain falls back to unity instead of poisoning samples.
        let (mut i, jitter) = ingest_with_gain(f32::NAN);
        feed(&mut i, PayloadType::Pcm, 0, &pcm_bytes(&[16384])).unwrap();
        let mut out = vec![0.0f32; 1];
        jitter.pop(&mut out);
        assert!((out[0] - 0.5).abs() < 1e-6, "NaN gain must not apply, got {}", out[0]);
    }

    fn ingest_with_gain(gain: f32) -> (Ingest, Arc<JitterBuffer>) {
        let jitter = Arc::new(JitterBuffer::new(8192));
        (Ingest::new(jitter.clone(), None, gain), jitter)
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
        let mut i = Ingest::new(jitter.clone(), None, 1.0);
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
