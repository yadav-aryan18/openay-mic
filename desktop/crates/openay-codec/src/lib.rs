//! OpenAY Mic audio codec: a strictly-configured libopus wrapper.
//!
//! Configuration is fixed per `shared/protocol.md`:
//!
//! - 48 kHz, mono
//! - `OPUS_APPLICATION_RESTRICTED_LOWDELAY`
//! - 10 ms frames (480 samples)
//! - default bitrate 32 kbps (configurable 16–96 kbps via [`OpusCodec::set_bitrate`])
//!
//! The wrapper is not thread-safe; use one [`OpusCodec`] per audio direction
//! or serialize access with a mutex.

use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Bitrate, Channels, MutSignals, SampleRate};

/// Sample rate in Hz (fixed by the protocol).
pub const SAMPLE_RATE: u32 = 48_000;
/// Channel count (mono).
pub const CHANNELS: u16 = 1;
/// Samples per frame: 10 ms at 48 kHz.
pub const FRAME_SAMPLES: usize = 480;
/// Default encoder bitrate in bits per second.
pub const DEFAULT_BITRATE: u32 = 32_000;
/// Minimum supported bitrate (per protocol: 16–96 kbps).
pub const MIN_BITRATE: u32 = 16_000;
/// Maximum supported bitrate.
pub const MAX_BITRATE: u32 = 96_000;

/// Errors produced by the codec.
#[derive(Debug)]
pub enum CodecError {
    /// The libopus call failed.
    Opus(audiopus::Error),
    /// `encode` was handed a frame that is not exactly 480 samples.
    BadFrameSize { expected: usize, got: usize },
    /// Requested bitrate is outside the protocol's 16–96 kbps range.
    BadBitrate(u32),
    /// `decode` was handed a packet with fewer than 1 byte.
    EmptyPacket,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Opus(e) => write!(f, "opus error: {e}"),
            CodecError::BadFrameSize { expected, got } => {
                write!(f, "bad frame size: expected {expected} samples, got {got}")
            }
            CodecError::BadBitrate(bps) => {
                write!(f, "bitrate {bps} bps outside supported range {MIN_BITRATE}..={MAX_BITRATE}")
            }
            CodecError::EmptyPacket => write!(f, "empty opus packet"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CodecError::Opus(e) => Some(e),
            _ => None,
        }
    }
}

impl From<audiopus::Error> for CodecError {
    fn from(e: audiopus::Error) -> Self {
        CodecError::Opus(e)
    }
}

/// Combined Opus encoder + decoder pair.
///
/// Both halves share the fixed protocol configuration.
pub struct OpusCodec {
    encoder: Encoder,
    decoder: Decoder,
    bitrate: u32,
}

impl OpusCodec {
    /// Create an encoder/decoder pair with the protocol configuration
    /// (48 kHz mono, restricted lowdelay, 32 kbps).
    pub fn new() -> Result<Self, CodecError> {
        let mut encoder = Encoder::new(
            SampleRate::Hz48000,
            Channels::Mono,
            Application::LowDelay,
        )?;
        encoder.set_bitrate(Bitrate::BitsPerSecond(DEFAULT_BITRATE as i32))?;
        let decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)?;
        Ok(OpusCodec {
            encoder,
            decoder,
            bitrate: DEFAULT_BITRATE,
        })
    }

    /// Set the encoder bitrate (16–96 kbps per the protocol).
    pub fn set_bitrate(&mut self, bps: u32) -> Result<(), CodecError> {
        if !(MIN_BITRATE..=MAX_BITRATE).contains(&bps) {
            return Err(CodecError::BadBitrate(bps));
        }
        self.encoder.set_bitrate(Bitrate::BitsPerSecond(bps as i32))?;
        self.bitrate = bps;
        Ok(())
    }

    /// Current encoder bitrate in bits per second.
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Encode exactly one 10 ms frame (480 i16 samples) into an Opus packet.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, CodecError> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(CodecError::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: pcm.len(),
            });
        }
        // 4000 bytes covers the absolute maximum Opus packet size.
        let mut out = vec![0u8; 4000];
        let n = self.encoder.encode(pcm, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// Decode one Opus packet into one 10 ms frame (480 i16 samples).
    ///
    /// Note: the decoder introduces the codec's inherent lookahead delay
    /// (a few ms at 48 kHz); a freshly started stream's first frames are
    /// affected. See the sine test for how the delay is measured.
    pub fn decode(&mut self, pkt: &[u8]) -> Result<Vec<i16>, CodecError> {
        if pkt.is_empty() {
            return Err(CodecError::EmptyPacket);
        }
        let packet = Packet::try_from(pkt)?;
        let mut out = vec![0i16; FRAME_SAMPLES];
        let n = self
            .decoder
            .decode(Some(packet), MutSignals::try_from(out.as_mut_slice())?, false)?;
        out.truncate(n);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 s of 440 Hz sine at 48 kHz mono, amplitude 0.9 FS (no clipping).
    fn sine_1s() -> Vec<i16> {
        let fs = 48_000.0f64;
        (0..48_000usize)
            .map(|i| {
                let t = i as f64 / fs;
                let v = (2.0 * std::f64::consts::PI * 440.0 * t).sin();
                (v * 0.9 * 32767.0) as i16
            })
            .collect()
    }

    #[test]
    fn encode_decode_440hz_sine_1s() {
        const FS: f64 = 32767.0;
        let pcm = sine_1s();
        assert_eq!(pcm.len(), 48_000);
        assert_eq!(pcm.len() % FRAME_SAMPLES, 0);
        let frame_count = pcm.len() / FRAME_SAMPLES;

        let mut codec = OpusCodec::new().expect("opus init");

        // Encode + decode frame by frame, timing each round trip.
        let mut decoded = Vec::with_capacity(pcm.len());
        let mut total_ns: u128 = 0;
        for frame in pcm.chunks_exact(FRAME_SAMPLES) {
            let t0 = std::time::Instant::now();
            let pkt = codec.encode(frame).expect("encode frame");
            let out = codec.decode(&pkt).expect("decode frame");
            total_ns += t0.elapsed().as_nanos();
            assert_eq!(out.len(), FRAME_SAMPLES, "decode output frame size");
            decoded.extend_from_slice(&out);
        }
        let avg_us = total_ns as f64 / frame_count as f64 / 1000.0;
        println!(
            "codec: {} frames, avg encode+decode {:.2} us/frame",
            frame_count, avg_us
        );

        // The codec inserts a lookahead delay (2.5 ms CELT at 48 kHz = 120
        // samples): decoded[i] corresponds to pcm[i - delay]. Additionally the
        // first decoded frame contains the decoder's startup transient (there
        // is no input history before the stream), so it is excluded from the
        // quality comparison. The delay is measured empirically on the
        // steady-state region so the test adapts to libopus versions.
        //
        // A pure tone is periodic (440 Hz -> 109.09 sample period), so the
        // RMS-vs-delay landscape has several equally "good" valleys (e.g. at
        // delays 120, 229, 338...). The physical CELT lookahead is 120
        // samples; delays below ~60 are impossible (the decoder cannot emit
        // samples before the input exists). Restricting the search to
        // [60, 1000) and taking the smallest well-aligned delay picks the
        // physical delay deterministically.
        const MIN_PHYSICAL_DELAY: usize = 60;
        const SKIP_FIRST_FRAME: usize = FRAME_SAMPLES;
        let mut rms_by_delay = Vec::new();
        for delay in MIN_PHYSICAL_DELAY..1000usize {
            let mut sum_sq = 0f64;
            let mut n = 0usize;
            for i in SKIP_FIRST_FRAME..decoded.len() {
                if i >= delay && i - delay < pcm.len() {
                    let e = decoded[i] as f64 - pcm[i - delay] as f64;
                    sum_sq += e * e;
                    n += 1;
                }
            }
            if n > 0 {
                rms_by_delay.push((delay, (sum_sq / n as f64).sqrt()));
            }
        }
        let best_rms = rms_by_delay
            .iter()
            .map(|&(_, r)| r)
            .fold(f64::MAX, f64::min);
        // A correctly aligned window has RMS ~0.5% FS; a misaligned window is
        // near the signal level itself (~64% FS). The gap is unambiguous.
        let good_rms = 0.03 * FS; // 3% FS
        assert!(
            best_rms < good_rms,
            "codec alignment implausible: best RMS {best_rms:.2} is not 'good' (< {good_rms:.2}); \
             the codec is not reproducing the input"
        );
        let best_delay = rms_by_delay
            .iter()
            .find(|&&(_, r)| r < good_rms)
            .map(|&(d, _)| d)
            .expect("at least one well-aligned delay in the physical range");
        println!(
            "codec: measured codec delay = {} samples ({:.1} ms)",
            best_delay,
            best_delay as f64 / SAMPLE_RATE as f64 * 1000.0
        );

        // Verify the error bounds on the steady-state region.
        let mut max_abs: f64 = 0.0;
        let mut sum_sq = 0.0;
        let mut n = 0usize;
        for i in SKIP_FIRST_FRAME..decoded.len() {
            if i >= best_delay && i - best_delay < pcm.len() {
                let e = (decoded[i] as f64 - pcm[i - best_delay] as f64).abs();
                max_abs = max_abs.max(e);
                sum_sq += e * e;
                n += 1;
            }
        }
        let rms = (sum_sq / n as f64).sqrt();

        assert!(
            max_abs < 1500.0,
            "max abs error {max_abs} exceeds 1500 (~4.5% FS)"
        );
        assert!(
            rms < 0.02 * FS,
            "RMS error {rms} exceeds 2% FS ({})",
            0.02 * FS
        );
        println!(
            "codec: n={n} compared samples, max abs error {max_abs:.1}, rms error {rms:.2} ({:.2}% FS)",
            rms / FS * 100.0
        );
    }

    #[test]
    fn bitrate_setter_clamps_range() {
        let mut codec = OpusCodec::new().unwrap();
        assert_eq!(codec.bitrate(), DEFAULT_BITRATE);
        codec.set_bitrate(16_000).unwrap();
        assert_eq!(codec.bitrate(), 16_000);
        codec.set_bitrate(96_000).unwrap();
        assert_eq!(codec.bitrate(), 96_000);
        assert!(matches!(
            codec.set_bitrate(15_999),
            Err(CodecError::BadBitrate(_))
        ));
        assert!(matches!(
            codec.set_bitrate(96_001),
            Err(CodecError::BadBitrate(_))
        ));
    }

    #[test]
    fn frame_size_enforced() {
        let mut codec = OpusCodec::new().unwrap();
        assert!(matches!(
            codec.encode(&[0i16; 479]),
            Err(CodecError::BadFrameSize { .. })
        ));
        assert!(matches!(
            codec.encode(&[0i16; 481]),
            Err(CodecError::BadFrameSize { .. })
        ));
        assert!(matches!(codec.decode(&[]), Err(CodecError::EmptyPacket)));
    }

    #[test]
    fn bitrate_affects_packet_size() {
        let pcm = sine_1s();
        let mut codec = OpusCodec::new().unwrap();
        codec.set_bitrate(32_000).unwrap();
        let small: Vec<usize> = pcm
            .chunks_exact(FRAME_SAMPLES)
            .map(|f| codec.encode(f).unwrap().len())
            .collect();
        codec.set_bitrate(96_000).unwrap();
        let large: Vec<usize> = pcm
            .chunks_exact(FRAME_SAMPLES)
            .map(|f| codec.encode(f).unwrap().len())
            .collect();
        let avg = |v: &[usize]| v.iter().sum::<usize>() as f64 / v.len() as f64;
        println!(
            "codec: avg pkt size @32k = {:.1} B, @96k = {:.1} B",
            avg(&small),
            avg(&large)
        );
        assert!(avg(&large) > avg(&small) * 1.5);
    }
}