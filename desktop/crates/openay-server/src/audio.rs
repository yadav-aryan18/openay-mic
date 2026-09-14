//! Platform audio output (feature `audio-output`).
//!
//! The cpal counterpart of [`crate::pw`]: instead of exposing a PipeWire
//! virtual microphone, the decoded stream is played on a real output device —
//! ALSA on Linux, WASAPI on Windows. On Windows there is no PipeWire and a
//! true virtual-microphone driver is out of scope; applications receive the
//! phone's audio through whatever the OS routes to the selected output
//! device (typically a virtual-audio-cable configured as the default
//! playback endpoint).
//!
//! # RT callback contract (mirrors `pw.rs` exactly)
//!
//! The realtime callback is the jitter buffer's single consumer and obeys
//! the same rules as the PipeWire RT callback:
//!
//! - **Prebuffer latch**: emit silence until `available >= target_samples`
//!   (then latch `streaming` on) or the latch is already on; once on, keep
//!   streaming until an underrun resets it.
//! - **Pop up to one quantum per callback** (here: the frame count the
//!   device asked for in this callback — the cpal analogue of PipeWire's
//!   graph quantum; on ALSA it varies with the recovered availability, on
//!   WASAPI it is the negotiated periodic block) and **zero-fill the
//!   remainder** with the format's silence value. A partial pop is normal
//!   operation (the quantum can exceed the prebuffer target), not an error.
//! - A **completely dry pop while streaming** is a real underrun: it is
//!   counted (`JitterBuffer::note_underrun`) and resets the latch so the
//!   next burst re-prebuffers instead of crackling.
//! - **Peak level**: fold `max |sample|` (over the popped samples, scaled to
//!   `0..=65535`) into the shared accumulator with a strict-max CAS loop;
//!   `EngineStatus::level_peak` reads it by exchange.
//!
//! No locks, no allocation, no logging inside the callback: all shared
//! state is `Arc<JitterBuffer>` + atomics, and the mono scratch buffer used
//! for channel conversion is allocated once at stream setup.
//!
//! # Lifecycle
//!
//! [`run_audio_output`] mirrors [`crate::pw::run_pipewire`]: blocking, to be
//! called on a dedicated OS thread. Setup failures are reported through
//! `setup_sender` (a single `Err` before the stream runs); on success the
//! sender is dropped without sending and the thread blocks until `quit`.
//! Stream errors (device unplug, host error) set `quit` so the engine
//! reaps the pipeline cleanly; the message goes to stderr.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig};
use openay_jitter::JitterBuffer;

use crate::SAMPLE_RATE;

/// Sample format's silence after mono conversion: f32/i16 zero, u16 mid-scale.
/// Zero-filling a `u16` buffer with literal 0 would be full-scale noise, so
/// the "zero-fill the remainder" rule fills with `T::SILENCE`, not `T::ZERO`.
trait FromMono: Copy {
    const SILENCE: Self;
    fn from_mono(sample: f32) -> Self;
}

impl FromMono for f32 {
    const SILENCE: Self = 0.0;
    fn from_mono(sample: f32) -> Self {
        sample
    }
}

impl FromMono for i16 {
    const SILENCE: Self = 0;
    fn from_mono(sample: f32) -> Self {
        f32_to_i16(sample)
    }
}

impl FromMono for u16 {
    const SILENCE: Self = 32_768;
    fn from_mono(sample: f32) -> Self {
        f32_to_u16(sample)
    }
}

/// Convert a linear f32 sample (protocol range `[-1.0, 1.0]`) to s16.
///
/// Scale 32767 (deterministic, matches the C++ test source's 0.8 * 32767
/// convention), round to nearest, clamp for the degenerate `s16` range
/// beyond full scale; a NaN input converts to 0 (saturating float->int cast).
fn f32_to_i16(sample: f32) -> i16 {
    (sample * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16
}

/// Convert a linear f32 sample to u16 (unsigned, mid-scale silence).
///
/// `0.0 -> 32768`, `+1.0 -> 65535`, `-1.0 -> 1`; clamped to `0..=65535`,
/// NaN converts to the silence value 32768 (saturating cast of NaN is 0,
/// so NaN is normalized explicitly).
fn f32_to_u16(sample: f32) -> u16 {
    if sample.is_nan() {
        return 32_768;
    }
    (sample * 32_767.0 + 32_768.0).round().clamp(0.0, 65_535.0) as u16
}

/// State shared between the engine and the RT output callback — the same
/// five fields as [`crate::pw::PwShared`], same atomics-only contract, plus
/// a one-shot stream-error slot written only by the non-RT error callback
/// (so a device unplug surfaces through `last_error` instead of ending the
/// run silently).
pub struct AudioShared {
    /// The audio samples produced by the network task.
    pub jitter: Arc<JitterBuffer>,
    /// Streaming latch: once the prebuffer target is reached we keep
    /// streaming until an underrun resets it.
    pub streaming: Arc<AtomicBool>,
    /// Set by the engine on Stop (or by a fatal stream error) to end the run.
    pub quit: Arc<AtomicBool>,
    /// `ceil(target_ms * 48_000 / 1000)` samples that must be buffered before
    /// the first real samples are emitted. Written by the depth-controller
    /// task (live retarget), read Relaxed by the RT callback — a callback
    /// must never block or spin.
    pub target_samples: Arc<AtomicU32>,
    /// Peak level accumulator (RT-safe): the callback folds `max |sample|`
    /// scaled to `0..=65535` into this atomic with a strict-max CAS loop;
    /// the status reader exchanges it on read.
    pub peak_level: Arc<AtomicU32>,
    /// One-shot stream-error message (device unplug, host error): set by the
    /// error callback right before it raises `quit`; `run_audio_output`
    /// turns it into the thread's `Err` so the reaper records `last_error`.
    pub stream_error: Arc<Mutex<Option<String>>>,
}

/// Case-insensitive substring device match (`needle` empty = matches any).
///
/// Setup-path only (never in the RT callback). Matching is on the device
/// name as reported by the host, lowercased with `to_lowercase` (Unicode
/// aware) on both sides.
fn name_matches(candidate: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    candidate.to_lowercase().contains(&needle.to_lowercase())
}

/// Names of the host's output devices (empty, without error, when the host
/// has none or cannot be enumerated — the CLI `--list-devices` consumer
/// treats an empty list as "nothing to select").
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices.filter_map(|d| d.name().ok()).collect()
}

/// Resolve the output device: `None` selects the host default; a substring
/// selects the first device whose name matches case-insensitively. No match
/// (or no devices at all) is a clean error naming the substring.
fn find_output_device(name_substring: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match name_substring {
        None => host
            .default_output_device()
            .context("no default audio output device on this host"),
        Some(needle) => {
            let devices = host
                .output_devices()
                .context("enumerating audio output devices")?;
            for device in devices {
                let Ok(name) = device.name() else {
                    continue;
                };
                if name_matches(&name, needle) {
                    return Ok(device);
                }
            }
            bail!("no audio output device matching {needle:?} (see --list-devices)")
        }
    }
}

/// Setup-time description of one output configuration the device supports
/// (a local mirror of cpal's `SupportedStreamConfigRange`, which has no
/// public constructor — keeping the selection policy a pure function makes
/// it unit-testable without an audio device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceConfigDesc {
    channels: u16,
    min_rate: u32,
    max_rate: u32,
    format: SampleFormat,
}

impl DeviceConfigDesc {
    /// True when the protocol-fixed 48 kHz rate is inside the range.
    fn covers_48khz(&self) -> bool {
        self.min_rate <= SAMPLE_RATE as u32 && self.max_rate >= SAMPLE_RATE as u32
    }

    /// Selection rank: convertible formats first (f32 preferred), then the
    /// fewest channels. `None` for formats we cannot produce from f32 mono.
    fn rank(&self) -> Option<(u8, u16)> {
        let format = match self.format {
            SampleFormat::F32 => 0u8,
            SampleFormat::I16 => 1,
            SampleFormat::U16 => 2,
            _ => return None,
        };
        Some((format, self.channels))
    }
}

/// Pick the best output configuration for the protocol stream: a format we
/// can convert to (f32 > i16 > u16), 48 kHz coverage, fewest channels (mono
/// if the device offers it, else the smallest upmix). Returns the index into
/// `descs`; `None` when nothing qualifies (a clean error at the call site).
fn choose_output_config(descs: &[DeviceConfigDesc]) -> Option<usize> {
    descs
        .iter()
        .enumerate()
        .filter(|(_, d)| d.covers_48khz())
        .filter_map(|(i, d)| d.rank().map(|r| (i, r)))
        .min_by_key(|&(_, rank)| rank)
        .map(|(i, _)| i)
}

/// Largest mono scratch the RT callback may need (frames). ALSA delivers
/// bursts of up to the whole configured ALSA buffer per callback (cpal
/// resizes the slice to the recovered availability), WASAPI delivers its
/// negotiated periodic block; 8192 frames (~170 ms) covers every realistic
/// host block, and a fixed upper bound keeps the callback allocation-free.
const MAX_SCRATCH_FRAMES: usize = 8192;

/// Open, configure, and start the output stream. Blocks are not accepted
/// here — this returns once `play()` succeeded; the stream runs on the
/// host's own audio thread.
fn open_output_stream(shared: &AudioShared, name_substring: Option<&str>) -> Result<Stream> {
    let device = find_output_device(name_substring)?;
    let device_name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());

    let mut ranges: Vec<(cpal::SupportedStreamConfigRange, DeviceConfigDesc)> = device
        .supported_output_configs()
        .with_context(|| format!("querying output configs of device {device_name:?}"))?
        .map(|range| {
            let desc = DeviceConfigDesc {
                channels: range.channels(),
                min_rate: range.min_sample_rate().0,
                max_rate: range.max_sample_rate().0,
                format: range.sample_format(),
            };
            (range, desc)
        })
        .collect();

    let idx = choose_output_config(&ranges.iter().map(|(_, d)| *d).collect::<Vec<_>>())
        .with_context(|| {
            format!(
                "device {device_name:?} supports none of f32/i16/u16 output at {SAMPLE_RATE} Hz"
            )
        })?;
    let (range, desc) = ranges.swap_remove(idx);

    // Pin the protocol rate inside the supported range and keep the host's
    // default buffer policy (the RT callback adapts to whatever block size
    // it is handed).
    let config: StreamConfig = range.with_sample_rate(SampleRate(SAMPLE_RATE as u32)).config();
    eprintln!(
        "openay-server: audio output device {device_name:?} ({} ch, {SAMPLE_RATE} Hz, {:?})",
        desc.channels, desc.format
    );

    // ALSA pacing (see build_stream): pin the device buffer so one callback
    // cannot demand more audio than the prebuffer latch holds. WASAPI and
    // other hosts keep the negotiated block; the callback adapts anyway.
    // (HostId::Alsa only exists on hosts that compile the ALSA backend.)
    #[cfg(target_os = "linux")]
    let pacing = if cpal::default_host().id() == cpal::HostId::Alsa {
        Some(cpal::BufferSize::Fixed(ALSA_DEVICE_BUFFER_FRAMES))
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let pacing = None;

    match desc.format {
        SampleFormat::F32 => build_stream::<f32>(device, &config, shared, desc.channels, pacing),
        SampleFormat::I16 => build_stream::<i16>(device, &config, shared, desc.channels, pacing),
        SampleFormat::U16 => build_stream::<u16>(device, &config, shared, desc.channels, pacing),
        other => bail!("unsupported output sample format {other:?} on {device_name:?}"),
    }
}

/// ALSA device buffer the output stream pins (frames): cpal's ALSA host
/// defaults to a ~25 ms period / ~100 ms buffer, so a single callback can
/// demand more audio than the prebuffer latch holds (the protocol's target
/// clamps at 20 ms) — that would drain the jitter buffer to zero every
/// cycle. 40 ms buffer = 10 ms period: every callback stays inside the
/// latch margin, and the depth controller's headroom is effective.
#[cfg(target_os = "linux")]
const ALSA_DEVICE_BUFFER_FRAMES: u32 = 1_920;

/// Build and start the output stream for one concrete sample format `T`,
/// wiring the RT callback (see the module docs for the contract). `pacing`
/// pins the device buffer size on hosts where the default pacing fights the
/// prebuffer latch; if the device rejects the exact size, the build retries
/// once with the host's default buffer policy.
fn build_stream<T: FromMono + cpal::SizedSample + Send + 'static>(
    device: cpal::Device,
    base_config: &StreamConfig,
    shared: &AudioShared,
    channels: u16,
    pacing: Option<cpal::BufferSize>,
) -> Result<Stream> {
    let mut config = base_config.clone();
    if let Some(buffer) = pacing {
        config.buffer_size = buffer;
    }
    // The scratch must cover the biggest request the device can make: the
    // negotiated Fixed size when we pinned one, otherwise the MAX floor
    // (a Default-policy host is clamped in-callback, never under-drained:
    // unpopped samples simply stay in the ring for the next callback).
    let scratch_frames = match config.buffer_size {
        cpal::BufferSize::Fixed(frames) => MAX_SCRATCH_FRAMES.max(frames as usize),
        cpal::BufferSize::Default => MAX_SCRATCH_FRAMES,
    };
    let make_err_callback = || {
        let quit_on_stream_error = shared.quit.clone();
        let stream_error = shared.stream_error.clone();
        move |err: cpal::StreamError| {
            // Host-level stream error (device unplug, driver reset): record
            // the cause and end the run cleanly — the engine reaps the
            // pipeline on `quit` exactly as it does for a PipeWire teardown,
            // and the reaper turns the recorded message into `last_error`
            // (GUI/CLI parity with setup failures).
            *stream_error
                .lock()
                .expect("audio stream error mutex poisoned") = Some(err.to_string());
            eprintln!("openay-server: audio output stream error: {err}");
            quit_on_stream_error.store(true, Ordering::Relaxed);
        }
    };

    // First attempt: the pinned pacing (when requested).
    let stream = match device.build_output_stream(
        &config,
        make_data_callback::<T>(shared, channels, scratch_frames),
        make_err_callback(),
        None,
    ) {
        Ok(stream) => stream,
        Err(err) => {
            // Retry once with the host's default buffer policy (devices
            // that cannot honor an exact size); a failure there is the
            // real setup error.
            let base = base_config.clone();
            anyhow::ensure!(
                config.buffer_size != base.buffer_size,
                "building output stream ({channels} ch, {base_config:?}): {err}"
            );
            eprintln!(
                "openay-server: audio output: device rejected buffer size \
                 {buffer:?}, retrying with the host default ({err})",
                buffer = config.buffer_size
            );
            device.build_output_stream(
                &base,
                make_data_callback::<T>(shared, channels, scratch_frames),
                make_err_callback(),
                None,
            )
            .with_context(|| {
                format!("building output stream ({channels} ch, {base_config:?})")
            })?
        }
    };
    stream
        .play()
        .context("starting output stream (device busy?)")?;
    Ok(stream)
}

/// Assemble the RT data callback for one stream attempt: preallocated mono
/// scratch plus cloned `Arc` handles (each attempt owns its own set; the
/// losing attempt's clones drop with its closure).
fn make_data_callback<T: FromMono + Send + 'static>(
    shared: &AudioShared,
    channels: u16,
    scratch_frames: usize,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static {
    let jitter = shared.jitter.clone();
    let streaming = shared.streaming.clone();
    let target_samples = shared.target_samples.clone();
    let peak_level = shared.peak_level.clone();
    // Sized from the negotiated buffer (floor: MAX_SCRATCH_FRAMES) so the
    // in-callback clamp below is provably dead for Fixed-size hosts.
    let mut scratch = vec![0.0f32; scratch_frames];
    move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
        fill_blocks(
            data,
            &jitter,
            &streaming,
            &target_samples,
            &peak_level,
            &mut scratch[..],
            channels,
        );
    }
}

/// The RT callback: fill `data` (interleaved `channels`-channel output)
/// from the jitter buffer. No allocation, no locking, no logging.
///
/// `scratch` is the preallocated mono staging buffer; a device request
/// larger than it (never observed in practice) clamps and zero-fills the
/// excess rather than allocating.
fn fill_blocks<T: FromMono>(
    data: &mut [T],
    jitter: &JitterBuffer,
    streaming: &AtomicBool,
    target_samples: &AtomicU32,
    peak_level: &AtomicU32,
    scratch: &mut [f32],
    channels: u16,
) {
    let channels = usize::from(channels.max(1));
    let frames = data.len() / channels;
    if frames == 0 {
        // Degenerate sub-frame request (cpal never produces one): keep the
        // "whole output slice defined" invariant anyway.
        data.fill(T::SILENCE);
        return;
    }
    // The quantum for this callback: everything the device asked for
    // (clamped to the scratch capacity).
    let quantum = frames.min(scratch.len());

    // Prebuffer latch — identical to pw.rs: stream when already streaming or
    // when enough audio has buffered for the target depth.
    let mut written = 0usize;
    if streaming.load(Ordering::Relaxed)
        || jitter.available() >= target_samples.load(Ordering::Relaxed) as usize
    {
        streaming.store(true, Ordering::Relaxed);
        written = jitter.pop(&mut scratch[..quantum]);
    }

    // Convert the popped mono samples into the interleaved output, writing
    // the same (converted) sample to every channel of a frame, and
    // zero-fill the remainder: either still prebuffering (silence is
    // expected) or the tail of a quantum the jitter buffer could not fill
    // yet (normal, the quantum can exceed the prebuffer target).
    let silence = T::SILENCE;
    let mut cursor = 0usize;
    for &sample in &scratch[..written] {
        let value = T::from_mono(sample);
        for _ in 0..channels {
            data[cursor] = value;
            cursor += 1;
        }
    }
    for _ in written..frames {
        for _ in 0..channels {
            data[cursor] = silence;
            cursor += 1;
        }
    }
    // Stragglers of a non-integral frame count (never produced by cpal, but
    // this keeps the whole output slice defined).
    data[cursor..].fill(silence);

    // Only a completely dry pop while streaming is a real underrun; it
    // resets the streaming latch so the stream re-prebuffers.
    if written == 0 && streaming.load(Ordering::Relaxed) {
        jitter.note_underrun();
        streaming.store(false, Ordering::Relaxed);
    }

    // Level metering (RT-safe): fold max |sample| over the popped samples
    // into the shared peak accumulator, scaled to 0..=65535, strict-max CAS.
    if written > 0 {
        let mut peak = 0u32;
        for &sample in &scratch[..written] {
            let scaled = (sample.abs() * 65_535.0) as u32;
            if scaled > peak {
                peak = scaled;
            }
        }
        let mut prev = peak_level.load(Ordering::Relaxed);
        while prev < peak {
            match peak_level.compare_exchange_weak(prev, peak, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
    }
}

/// Run the audio output until `quit` is set. Blocking; call on a dedicated
/// OS thread. Mirrors [`crate::pw::run_pipewire`]: `setup_sender` receives a
/// single `Err` if setup fails before the stream runs; on success it is
/// dropped without sending (the engine infers success from the thread
/// continuing to run).
pub fn run_audio_output(
    shared: AudioShared,
    output_device: Option<String>,
    setup_sender: mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let result = open_output_stream(&shared, output_device.as_deref());
    if let Err(e) = &result {
        let _ = setup_sender.send(Err(format!("{e:#}")));
    }
    let stream = result?;

    // The cpal stream runs on the host's audio thread; this thread just
    // keeps the stream handle alive and polls `quit` (Stop / stream error).
    while !shared.quit.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
    // Dropping the stream stops the callbacks and joins the audio thread.
    drop(stream);
    // A recorded stream error (device unplug, host error) must reach
    // `last_error` — otherwise the run ends looking like a clean Stop.
    if let Some(err) = shared
        .stream_error
        .lock()
        .expect("audio stream error mutex poisoned")
        .take()
    {
        bail!("audio output stream error: {err}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- device-substring matcher ---------------------------------------

    #[test]
    fn name_matches_is_case_insensitive_substring() {
        assert!(name_matches(
            "Speakers (Realtek(R) Audio)",
            "realtek"
        ));
        assert!(name_matches(
            "Speakers (Realtek(R) Audio)",
            "REALTEK(R) AUDIO"
        ));
        assert!(name_matches("pipewire", "pipe"));
        assert!(name_matches("alsa_output.pci-0000_00_1f.3.analog-stereo", "analog-stereo"));
    }

    #[test]
    fn name_matches_rejects_non_substrings() {
        assert!(!name_matches("Speakers (Realtek)", "hdmi"));
        assert!(!name_matches("HDMI Output", "speakers"));
        assert!(!name_matches("pip", "pipewire"), "needle longer than name");
    }

    #[test]
    fn name_matches_empty_needle_matches_anything() {
        assert!(name_matches("anything", ""));
        assert!(name_matches("", ""));
        assert!(!name_matches("", "x"), "empty name does not match a real needle");
    }

    // --- f32 -> i16/u16 conversion ---------------------------------------

    #[test]
    fn f32_to_i16_known_values() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32_767);
        assert_eq!(f32_to_i16(-1.0), -32_767);
        assert_eq!(f32_to_i16(0.5), 16_384); // 0.5 * 32767 = 16383.5 -> rounds up
        assert_eq!(f32_to_i16(-0.5), -16_384);
        // Out-of-range inputs clamp (guard against blown-up samples).
        assert_eq!(f32_to_i16(1.5), 32_767);
        assert_eq!(f32_to_i16(-2.0), -32_768);
    }

    #[test]
    fn f32_to_i16_nan_is_silence() {
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(f32_to_i16(f32::INFINITY), 32_767);
        assert_eq!(f32_to_i16(f32::NEG_INFINITY), -32_768);
    }

    #[test]
    fn f32_to_u16_known_values() {
        assert_eq!(f32_to_u16(0.0), 32_768, "silence is mid-scale");
        assert_eq!(f32_to_u16(1.0), 65_535);
        assert_eq!(f32_to_u16(-1.0), 1);
        assert_eq!(f32_to_u16(0.5), 49_152); // 0.5*32767 + 32768 = 49151.5 -> 49152
        assert_eq!(f32_to_u16(-0.5), 16_385); // -16383.5 + 32768 = 16384.5, rounds away from zero
        assert_eq!(f32_to_u16(2.0), 65_535, "clamped");
        assert_eq!(f32_to_u16(-2.0), 0, "clamped");
    }

    #[test]
    fn f32_to_u16_nan_is_silence() {
        assert_eq!(f32_to_u16(f32::NAN), 32_768);
    }

    #[test]
    fn silence_constants_match_conversion() {
        assert_eq!(<f32 as FromMono>::SILENCE, 0.0);
        assert_eq!(<i16 as FromMono>::SILENCE, 0);
        assert_eq!(<u16 as FromMono>::SILENCE, 32_768);
        assert_eq!(i16::from_mono(0.0), <i16 as FromMono>::SILENCE);
        assert_eq!(u16::from_mono(0.0), <u16 as FromMono>::SILENCE);
    }

    // --- output config selection -----------------------------------------

    fn desc(channels: u16, format: SampleFormat) -> DeviceConfigDesc {
        DeviceConfigDesc {
            channels,
            min_rate: SAMPLE_RATE as u32,
            max_rate: SAMPLE_RATE as u32,
            format,
        }
    }

    #[test]
    fn choose_prefers_f32_then_fewest_channels() {
        let descs = [
            desc(2, SampleFormat::I16),
            desc(2, SampleFormat::F32),
            desc(1, SampleFormat::I16),
        ];
        let idx = choose_output_config(&descs).expect("a choice exists");
        assert_eq!(descs[idx].format, SampleFormat::F32);
        assert_eq!(descs[idx].channels, 2);
    }

    #[test]
    fn choose_prefers_mono_within_same_format() {
        let descs = [desc(2, SampleFormat::F32), desc(1, SampleFormat::F32)];
        let idx = choose_output_config(&descs).expect("a choice exists");
        assert_eq!(descs[idx].channels, 1);
    }

    #[test]
    fn choose_falls_back_to_i16_and_u16() {
        let descs = [desc(2, SampleFormat::U16), desc(2, SampleFormat::I16)];
        let idx = choose_output_config(&descs).expect("a choice exists");
        assert_eq!(descs[idx].format, SampleFormat::I16);

        let descs = [desc(2, SampleFormat::U16)];
        let idx = choose_output_config(&descs).expect("u16 is the only convertible option");
        assert_eq!(descs[idx].format, SampleFormat::U16);
    }

    #[test]
    fn choose_rejects_unconvertible_formats_and_wrong_rates() {
        // Only 44.1 kHz available: the protocol rate is fixed, so nothing
        // qualifies and the caller reports a clean error.
        let descs = [DeviceConfigDesc {
            channels: 2,
            min_rate: 44_100,
            max_rate: 44_100,
            format: SampleFormat::F32,
        }];
        assert_eq!(choose_output_config(&descs), None);

        // Non-convertible format at 48 kHz: still no choice.
        let descs = [desc(2, SampleFormat::I8)];
        assert_eq!(choose_output_config(&descs), None);

        assert_eq!(choose_output_config(&[]), None, "no configs at all");
    }

    #[test]
    fn choose_accepts_48k_inside_a_rate_range() {
        let descs = [DeviceConfigDesc {
            channels: 2,
            min_rate: 8_000,
            max_rate: 96_000,
            format: SampleFormat::F32,
        }];
        assert!(choose_output_config(&descs).is_some());
    }
}
