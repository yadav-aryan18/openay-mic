//! PipeWire virtual microphone source (feature `pipewire` only).
//!
//! Exposes the jitter buffer as a `Audio/Source/Virtual` node named
//! `openay_mic`, F32LE mono 48 kHz, driven by a real-time process callback
//! (RT_PROCESS). The callback is called from PipeWire's data loop thread and
//! must not lock, allocate, or log: all shared state is
//! [`Arc<JitterBuffer>`] + atomics.
//!
//! Shutdown: the main thread sets the `quit` flag; the loop is driven
//! manually with a 50 ms poll timeout (the `MainLoop` itself is `!Send` in
//! pipewire-rs 0.8, so it is never touched cross-thread).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{Context, Result};
use openay_jitter::JitterBuffer;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::pod::Pod;

/// Sample rate of the virtual source (fixed by the wire protocol).
pub const SAMPLE_RATE: u32 = 48_000;
/// Channel count of the virtual source.
pub const CHANNELS: u32 = 1;

/// State shared between the network task (producer) and the RT process
/// callback (consumer). Atomics only — the callback never blocks.
pub struct PwShared {
    /// The audio samples produced by the network task.
    pub jitter: Arc<JitterBuffer>,
    /// Streaming latch: once the prebuffer is filled we keep streaming until
    /// an underrun resets it.
    pub streaming: Arc<AtomicBool>,
    /// Set by the main thread after Ctrl-C to stop the loop.
    pub quit: Arc<AtomicBool>,
    /// `ceil(target_ms * 48_000 / 1000)` samples that must be buffered before
    /// the first real samples are emitted.
    pub target_samples: usize,
}

/// Run the PipeWire source until `quit` is set. Blocking; call on a
/// dedicated OS thread.
///
/// `setup_sender` receives a single `Err` message if setup fails before the
/// main loop starts; on success the sender is dropped without sending (the
/// main thread infers success from the thread continuing to run).
pub fn run_pipewire(
    shared: PwShared,
    setup_sender: mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let result = setup_source(shared);
    if let Err(e) = &result {
        let _ = setup_sender.send(Err(format!("{e:#}")));
    }
    result
}

fn setup_source(shared: PwShared) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None).context("creating main loop")?;
    let context = pw::context::Context::new(&mainloop).context("creating context")?;
    let core = context
        .connect(None)
        .context("connecting to PipeWire daemon")?;

    let stream = pw::stream::Stream::new(
        &core,
        "openay-mic",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Source",
            *pw::keys::MEDIA_ROLE => "DSP",
            *pw::keys::MEDIA_CLASS => "Audio/Source/Virtual",
            *pw::keys::NODE_NAME => "openay_mic",
            *pw::keys::NODE_DESCRIPTION => "OpenAY Mic",
        },
    )
    .context("creating stream")?;

    let _listener = stream
        .add_local_listener_with_user_data(shared)
        .process(process_callback)
        .register()
        .context("registering stream listener")?;

    // F32LE mono 48 kHz.
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS);

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .context("serializing format pod")?
    .0
    .into_inner();

    let pod = Pod::from_bytes(&values).context("parsing format pod")?;
    let mut params: [&Pod; 1] = [&pod];

    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("connecting stream")?;

    // Drive the loop manually with a short poll timeout so the `quit` flag
    // is honored promptly; the RT process callback is unaffected (it runs on
    // PipeWire's data loop thread).
    let loop_ref = mainloop.loop_();
    while !shared.quit.load(Ordering::Relaxed) {
        if loop_ref.iterate(Duration::from_millis(50)) < 0 {
            break;
        }
    }
    Ok(())
}

/// Real-time process callback: fill the PipeWire buffer from the jitter
/// buffer. No allocation, no locking, no logging.
///
/// Policy (per plan): emit silence until `available >= target_samples`, then
/// pop as much as fits; if a callback has to zero-fill any part of the
/// buffer, count one underrun and drop back to the silence (prebuffer) state
/// so the next burst starts clean instead of crackling.
fn process_callback(stream: &pw::stream::StreamRef, shared: &mut PwShared) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }
    let data = &mut datas[0];
    let Some(bytes) = data.data() else {
        return;
    };
    let n_f32 = bytes.len() / 4;
    // SAFETY: `bytes` is a &mut [u8] over the mapped buffer (MAP_BUFFERS),
    // non-null (checked above) and its length is a multiple of 4 because the
    // negotiated format is F32LE. PipeWire's mempool aligns buffers to at
    // least 16 bytes, so the pointer is 4-byte aligned for f32 access.
    let samples: &mut [f32] =
        unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<f32>(), n_f32) };

    let mut written = 0usize;
    if shared.streaming.load(Ordering::Relaxed)
        || shared.jitter.available() >= shared.target_samples
    {
        shared.streaming.store(true, Ordering::Relaxed);
        written = shared.jitter.pop(samples);
    }
    if written < samples.len() {
        // Zero-fill: either still prebuffering, or an underrun. One underrun
        // is counted per callback that had to zero-fill, and the streaming
        // latch is reset so we re-prebuffer.
        samples[written..].fill(0.0);
        if written > 0 {
            shared.jitter.note_underrun();
        }
        shared.streaming.store(false, Ordering::Relaxed);
    }

    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = (4 * CHANNELS) as i32;
    *chunk.size_mut() = (4 * samples.len()) as i32;
}
