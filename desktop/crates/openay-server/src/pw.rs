//! PipeWire virtual microphone source (feature `pipewire` only).
//!
//! Architecture (the canonical PipeWire virtual-mic pattern, same as
//! module-echo-cancel):
//!
//! - An **internal producer** stream (`openay_engine`, an output stream over
//!   the jitter buffer) that is *not* exposed as a public source: it carries
//!   no `media.class`/`media.category`/`media.role`, so WirePlumber never
//!   exports it and it has no public ports of its own.
//! - A **public source node** `openay_mic` created via the `adapter` factory
//!   wrapping `support.null-audio-sink` with `media.class =
//!   Audio/Source/Virtual`. The null-sink is a graph driver, so the graph is
//!   always scheduled and continuously pulls data from the engine stream;
//!   recorders see a normal source with ports and can link to it (a portless
//!   suspended node would never be offered by the Pulse layer — the
//!   chicken-and-egg this architecture avoids).
//! - A **link-factory link** between the engine's stream node and the
//!   null-sink node; PipeWire negotiates the ports on activation. The Node
//!   and Link handles are kept alive for the process lifetime.
//!
//! Two timing traps are handled explicitly:
//!
//! - The link-factory resolves ports synchronously on the daemon and only
//!   reports failure via an async error event (invisible to
//!   [`pw::core::Core::create_object`], which returns a proxy optimistically).
//!   A non-dynamic node (the null-sink) that has no registered port yet makes
//!   the factory fail with `ENOSPC` ("no more port available"). We therefore
//!   wait for the null-sink's input port to be registered (node-info events)
//!   before linking, and verify the link via its own info event, retrying if
//!   it does not appear.
//! - `object.linger` is deliberately NOT set: the Node/Link proxies are kept
//!   alive for the whole process lifetime anyway, and lingering objects
//!   survive a stopped/crashed server as zombie `openay_mic` sources that
//!   pollute the graph and break name-based targeting (`pw-cat --target
//!   openay_mic`) on the next run. Without linger, a graceful shutdown
//!   removes the objects with the client.
//!
//! The RT process callback (RT_PROCESS) runs on the graph's data-loop thread
//! and must not lock, allocate, or log: all shared state is
//! [`Arc<JitterBuffer>`] + atomics.
//!
//! Shutdown: the main thread sets the `quit` flag; the loop is driven
//! manually with a 50 ms poll timeout (the `MainLoop` itself is `!Send` in
//! pipewire-rs 0.8, so it is never touched cross-thread).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use openay_jitter::JitterBuffer;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::pod::Pod;

/// Sample rate of the virtual source (fixed by the wire protocol).
pub const SAMPLE_RATE: u32 = 48_000;
/// Channel count of the virtual source.
pub const CHANNELS: u32 = 1;

/// How long to wait (after `connect`) for the server to create the stream's
/// node before giving up.
const NODE_ID_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for a link-factory link to be confirmed before retrying.
const LINK_CONFIRM_TIMEOUT: Duration = Duration::from_millis(500);
/// Maximum link-factory attempts.
const MAX_LINK_ATTEMPTS: u32 = 10;

/// Default graph quantum request: 10 ms — one wire frame per callback. A
/// quantum only shrinks if every node in the driven chain asks for it, so it
/// is requested on BOTH the engine stream and the null-sink driver (the
/// daemon default ~21 ms makes the consumer pop 2 wire frames per callback,
/// which drained the buffer to zero slack between pops and showed up as
/// mid-sentence dropouts). `OPENAY_NODE_LATENCY` overrides with a PipeWire
/// fraction (`960/48000`) or a bare sample count; set it to `1024/48000` to
/// restore the daemon-default cadence.
///
/// Pure setup-time configuration: properties are passed at object creation,
/// so the RT callback path is unaffected.
pub const DEFAULT_NODE_LATENCY: &str = "480/48000";

fn node_latency_property() -> Option<(&'static str, String)> {
    node_latency_for(std::env::var("OPENAY_NODE_LATENCY").ok().as_deref())
}

/// Pure decision so tests need no env mutation: an absent or blank override
/// selects [`DEFAULT_NODE_LATENCY`], anything non-blank goes through
/// [`parse_node_latency`].
fn node_latency_for(raw: Option<&str>) -> Option<(&'static str, String)> {
    match raw {
        Some(r) if !r.trim().is_empty() => parse_node_latency(Some(r)),
        _ => Some(("node.latency", DEFAULT_NODE_LATENCY.to_string())),
    }
}

fn parse_node_latency(raw: Option<&str>) -> Option<(&'static str, String)> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    // Accept "N/DEN" or a bare sample count N (denominator = our rate).
    let value = if let Some((num, den)) = raw.split_once('/') {
        match (num.trim().parse::<u64>(), den.trim().parse::<u64>()) {
            (Ok(n), Ok(d)) if n > 0 && d > 0 => format!("{n}/{d}"),
            _ => {
                eprintln!("openay-server: ignoring invalid OPENAY_NODE_LATENCY {raw:?}");
                return None;
            }
        }
    } else {
        match raw.parse::<u64>() {
            Ok(n) if n > 0 => format!("{n}/{SAMPLE_RATE}"),
            _ => {
                eprintln!("openay-server: ignoring invalid OPENAY_NODE_LATENCY {raw:?}");
                return None;
            }
        }
    };
    Some(("node.latency", value))
}

/// State shared between the network task (producer) and the RT process
/// callback (consumer). Atomics only — the callback never blocks.
///
/// `Clone` is derived because the listener's user data is handed over by
/// value to `add_local_listener_with_user_data`; the clone shares the same
/// `Arc`s (jitter/streaming/quit), so the callback and the loop observe the
/// exact same atomic state.
#[derive(Clone)]
pub struct PwShared {
    /// The audio samples produced by the network task.
    pub jitter: Arc<JitterBuffer>,
    /// Streaming latch: once the prebuffer is filled we keep streaming until
    /// an underrun resets it.
    pub streaming: Arc<AtomicBool>,
    /// Set by the main thread after Ctrl-C to stop the loop.
    pub quit: Arc<AtomicBool>,
    /// `ceil(target_ms * 48_000 / 1000)` samples that must be buffered before
    /// the first real samples are emitted. Written by the depth-controller
    /// task (live retarget), read Relaxed by the RT callback — a callback
    /// must never block or spin.
    pub target_samples: Arc<AtomicU32>,
    /// Peak level accumulator (RT-safe): the process callback folds
    /// `max |sample|` scaled to `0..=65535` into this atomic with a
    /// strict-max CAS loop; the status reader exchanges it on read
    /// (each snapshot consumes the interval).
    pub peak_level: Arc<AtomicU32>,
    /// Last sample emitted to the graph (f32 bits) — the hold value for
    /// dry-quantum concealment. Written by the RT callback, read by the RT
    /// callback (single consumer), Relaxed everywhere.
    pub last_sample: Arc<AtomicU32>,
    /// Consecutive fully-dry quanta while streaming. The RT callback
    /// increments it on a dry pop and clears it the moment data flows; a
    /// sustained run (>= [`DRY_RESET_QUANTA`]) resets the streaming latch so
    /// the stream re-prebuffers. A brief dry blip is instead concealed
    /// (hold + decay), which is why single blips do not count as underruns
    /// for the depth controller.
    pub dry_quanta: Arc<AtomicU32>,
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

    // Internal producer stream: deliberately NOT a public source (no
    // media.class/media.category/media.role — WirePlumber would otherwise
    // export it as a standalone node that would stay suspended with no
    // ports). The public source is the null-sink created below.
    let mut stream_props = properties! {
        *pw::keys::NODE_NAME => "openay_engine",
        *pw::keys::NODE_DESCRIPTION => "OpenAY Mic engine",
    };
    if let Some((key, value)) = node_latency_property() {
        eprintln!("openay-server: requesting {key} = {value}");
        stream_props.insert(key, value);
    }
    let stream =
        pw::stream::Stream::new(&core, "openay-mic", stream_props).context("creating stream")?;

    let _listener = stream
        .add_local_listener_with_user_data(shared.clone())
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

    // `Pod::from_bytes` returns a reference that borrows the serialized
    // bytes, so `params` holds references that stay valid for `connect`.
    let pod: &Pod = Pod::from_bytes(&values).context("parsing format pod")?;
    let mut params: [&Pod; 1] = [pod];

    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            // Deliberately no AUTOCONNECT: an output stream with that flag
            // gets auto-linked to the default sink, occupying its only
            // output port and leaving nothing for the explicit link to the
            // null-sink below (the link-factory would then fail).
            pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("connecting stream")?;

    let loop_ref = mainloop.loop_();

    // The stream's node id is SPA_ID_INVALID until the server has processed
    // the connect; iterate (roundtrip) until it becomes valid.
    let deadline = Instant::now() + NODE_ID_TIMEOUT;
    while stream.node_id() == pw::constants::ID_ANY {
        if Instant::now() >= deadline {
            bail!("stream node id never became valid (AUTOCONNECT failed?)");
        }
        if loop_ref.iterate(Duration::from_millis(10)) < 0 {
            bail!("main loop iterate failed while waiting for the stream node");
        }
    }
    let stream_id = stream.node_id();
    eprintln!(
        "openay-server: engine stream node id = {stream_id}, creating null-audio-sink source"
    );

    // Public source node: an adapter wrapping support.null-audio-sink. The
    // null-sink carries the graph-driver flag, so the graph is always
    // scheduled and keeps driving our stream's process() callback; without a
    // driver the engine node would stay suspended with zero ports and
    // nothing could link to it. Keep the Node handle alive for the process
    // lifetime (no object.linger: lingering nodes survive server shutdown as
    // zombie sources — see module docs).
    let mut sink_props = properties! {
        "factory.name" => "support.null-audio-sink",
        *pw::keys::MEDIA_CLASS => "Audio/Source/Virtual",
        *pw::keys::NODE_NAME => "openay_mic",
        *pw::keys::NODE_DESCRIPTION => "OpenAY Mic",
        "audio.position" => "[ FL ]",
    };
    // The null-sink is the graph driver: without its own node.latency
    // request the quantum stays at the daemon default even if the stream
    // asks for less.
    if let Some((key, value)) = node_latency_property() {
        sink_props.insert(key, value);
    }
    let source_node = core
        .create_object::<pw::node::Node>("adapter", &sink_props)
        .context("creating null-audio-sink source")?;

    // The proxy id (upcast_ref().id()) is the client-side proxy-map id, NOT
    // the node's global id; the node info event carries the real one. Capture
    // it so the link-factory below can reference the actual global. The link
    // is only created once the sink has enumerated its input ports: a
    // node-level link-factory created earlier resolves no ports and is
    // silently discarded by the daemon.
    let sink_id = Rc::new(Cell::new(0u32));
    let sink_ports = Rc::new(Cell::new(0u32));
    let sink_id_cb = sink_id.clone();
    let sink_ports_cb = sink_ports.clone();
    let _node_listener = source_node
        .add_listener_local()
        .info(move |info| {
            if info.id() != 0 {
                sink_id_cb.set(info.id());
                sink_ports_cb.set(info.n_input_ports());
            }
        })
        .register();
    let deadline = Instant::now() + NODE_ID_TIMEOUT;
    while sink_id.get() == 0 || sink_ports.get() == 0 {
        if Instant::now() >= deadline {
            bail!("null-sink node id/ports never became valid");
        }
        if loop_ref.iterate(Duration::from_millis(10)) < 0 {
            bail!("main loop iterate failed while waiting for the null-sink node");
        }
    }
    let sink_id = sink_id.get();
    eprintln!("openay-server: null-sink node id = {sink_id}");

    // Node-level link between the engine stream and the null-sink; PipeWire
    // negotiates the actual ports on activation. The link-factory resolves
    // the ports synchronously on the daemon and only reports failure as an
    // async error event, so the link is verified via its info event (retry
    // on silence). Keep the returned Link handle alive for the process
    // lifetime.
    let _link = create_link_verified(&core, loop_ref, stream_id, sink_id)
        .context("linking engine stream to null-sink")?;

    // Drive the loop manually with a short poll timeout so the `quit` flag
    // is honored promptly; the RT process callback is unaffected (it runs on
    // PipeWire's data loop thread).
    while !shared.quit.load(Ordering::Relaxed) {
        if loop_ref.iterate(Duration::from_millis(50)) < 0 {
            break;
        }
    }
    Ok(())
}

/// Create the engine -> null-sink link and verify it actually materialized.
///
/// `pw_core_create_object` returns a proxy optimistically: if the daemon's
/// link-factory fails (e.g. `ENOSPC` because a non-dynamic node has no free
/// port), the error is only sent as an async event on the resource, which
/// pipewire-rs does not surface. The link's info event (with matching node
/// ids) is the only reliable success signal, so we wait for it and retry —
/// each attempt failing cleanly because a failed factory creation leaves no
/// link behind.
fn create_link_verified(
    core: &pw::core::Core,
    loop_ref: &pw::loop_::LoopRef,
    stream_id: u32,
    sink_id: u32,
) -> Result<pw::link::Link> {
    for attempt in 0..MAX_LINK_ATTEMPTS {
        let confirmed = Rc::new(Cell::new(false));
        let confirmed_cb = confirmed.clone();

        let link = core
            .create_object::<pw::link::Link>(
                "link-factory",
                &properties! {
                    "link.output.node" => stream_id.to_string(),
                    "link.input.node" => sink_id.to_string(),
                },
            )
            .context("creating link-factory object")?;

        let _link_listener = link
            .add_listener_local()
            .info(move |info| {
                if info.output_node_id() == stream_id && info.input_node_id() == sink_id {
                    confirmed_cb.set(true);
                }
            })
            .register();

        let wait_until = Instant::now() + LINK_CONFIRM_TIMEOUT;
        while !confirmed.get() {
            if Instant::now() >= wait_until {
                break;
            }
            if loop_ref.iterate(Duration::from_millis(10)) < 0 {
                break;
            }
        }
        if confirmed.get() {
            eprintln!("openay-server: engine -> null-sink link created");
            return Ok(link);
        }

        // The factory never created the link (or the info event did not
        // arrive in time); drop the listener and the proxy, then retry.
        drop(_link_listener);
        let _ = core.destroy_object(link);
        eprintln!(
            "openay-server: link creation attempt {} not confirmed; retrying",
            attempt + 1
        );
    }
    bail!("could not create engine -> null-sink link after {MAX_LINK_ATTEMPTS} attempts")
}

/// Real-time process callback: fill the PipeWire buffer from the jitter
/// buffer. No allocation, no locking, no logging.
///
/// Policy: emit silence until `available >= target_samples`, then pop as
/// much as fits and keep streaming. The graph quantum (10 ms requested) may
/// still exceed the momentary buffered amount, so a partial pop —
/// zero-filling the tail of a quantum — is normal operation and does not
/// stop streaming.
///
/// Dryness is *concealed*, not hard-cut: a fully dry pop while streaming
/// holds the last emitted sample and decays it toward silence over
/// consecutive dry quanta (a hard zero-cut mid-word is audible as
/// truncation; a fast decaying hold reads as a soft dropout). Only a
/// sustained dry run ([`DRY_RESET_QUANTA`]) counts as a real underrun:
/// `note_underrun` fires once and the latch drops so the stream
/// re-prebuffers with silence — continuous with the decay that preceded it.
///
/// The dequeued buffer's data slice can be much larger than one quantum
/// (the negotiated pool block may hold 16384 samples while the graph's
/// quantum is smaller). We therefore fill at most one quantum per callback
/// and advertise exactly that in `chunk.size`; claiming the whole slice
/// would make the sink consume ~16 quanta per callback and stall the stream.
const QUANTUM_SAMPLES: usize = 1024;
/// Fully-dry quanta tolerated (with decaying hold) before the stream latch
/// drops and the engine's depth controller raises the prebuffer target.
/// At the default 10 ms quantum this is ~50 ms of concealment.
const DRY_RESET_QUANTA: u32 = 5;
/// Fade-in length after a dry episode, in samples (~0.7 ms) — masks the
/// step from the decayed hold back to live signal.
const FADE_IN_SAMPLES: usize = 32;

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

    let fill = QUANTUM_SAMPLES.min(n_f32);
    let mut written = 0usize;
    if shared.streaming.load(Ordering::Relaxed)
        || shared.jitter.available() >= shared.target_samples.load(Ordering::Relaxed) as usize
    {
        shared.streaming.store(true, Ordering::Relaxed);
        written = shared.jitter.pop(&mut samples[..fill]);
    }
    if written < fill {
        let dry = shared.dry_quanta.load(Ordering::Relaxed);
        let hold = f32::from_bits(shared.last_sample.load(Ordering::Relaxed));
        if written == 0 && shared.streaming.load(Ordering::Relaxed) {
            // Fully dry while streaming: hold the last sample, decaying by
            // half per consecutive dry quantum, then silence.
            let gain = 0.5f32.powi(dry.min(16) as i32);
            let value = if hold.abs() * gain < 1e-6 { 0.0 } else { hold * gain };
            samples[written..fill].fill(value);
            let next = dry + 1;
            if next >= DRY_RESET_QUANTA {
                // Sustained dryness: count the underrun once, drop the latch,
                // and let the re-prebuffer take over (from near-silence, so
                // the transition is inaudible).
                shared.jitter.note_underrun();
                shared.streaming.store(false, Ordering::Relaxed);
                shared.dry_quanta.store(0, Ordering::Relaxed);
                shared.last_sample.store(0.0f32.to_bits(), Ordering::Relaxed);
            } else {
                shared.dry_quanta.store(next, Ordering::Relaxed);
            }
        } else {
            // Prebuffering silence, or the tail of a partially-filled
            // quantum: hold the last written sample across the tail (data
            // is flowing; this bridge is shorter than one quantum).
            let bridge = if written > 0 { samples[written - 1] } else { hold };
            samples[written..fill].fill(bridge);
            if written > 0 {
                shared.last_sample
                    .store(samples[written - 1].to_bits(), Ordering::Relaxed);
            }
        }
    } else {
        // Data filled the quantum. After a dry episode, fade the live
        // signal in over FADE_IN_SAMPLES to mask the step from the decayed
        // hold; otherwise continue at unity.
        let dry = shared.dry_quanta.swap(0, Ordering::Relaxed);
        if dry > 0 {
            let fade = FADE_IN_SAMPLES.min(written);
            for (i, s) in samples[..fade].iter_mut().enumerate() {
                *s *= i as f32 / fade as f32;
            }
        }
        shared.last_sample
            .store(samples[written - 1].to_bits(), Ordering::Relaxed);
    }

    // Level metering (RT-safe: no locks, no allocation, no logging). Fold
    // max |sample| over the popped samples into the shared peak accumulator,
    // scaled to 0..=65535, with a strict-max CAS loop.
    if written > 0 {
        let mut peak = 0u32;
        for &s in &samples[..written] {
            let v = (s.abs() * 65_535.0) as u32;
            if v > peak {
                peak = v;
            }
        }
        let cell = &shared.peak_level;
        let mut prev = cell.load(Ordering::Relaxed);
        while prev < peak {
            match cell.compare_exchange_weak(prev, peak, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
    }

    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = (4 * CHANNELS) as i32;
    *chunk.size_mut() = (4 * fill) as u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_latency_unset_or_empty_uses_default() {
        let default = Some(("node.latency", DEFAULT_NODE_LATENCY.to_string()));
        assert_eq!(node_latency_for(None), default, "unset env: 10 ms default");
        assert_eq!(node_latency_for(Some("")), default, "blank env: 10 ms default");
        assert_eq!(node_latency_for(Some("  ")), default, "blank env: 10 ms default");
    }

    #[test]
    fn node_latency_env_overrides_the_default() {
        assert_eq!(
            node_latency_for(Some("960/48000")),
            Some(("node.latency", "960/48000".to_string()))
        );
        assert_eq!(
            node_latency_for(Some(" 1024 / 48000 ")),
            Some(("node.latency", "1024/48000".to_string()))
        );
    }

    #[test]
    fn node_latency_fraction_and_bare_count() {
        assert_eq!(
            parse_node_latency(Some("480/48000")),
            Some(("node.latency", "480/48000".to_string()))
        );
        // Bare sample count takes the protocol rate as denominator.
        assert_eq!(
            parse_node_latency(Some("256")),
            Some(("node.latency", format!("256/{SAMPLE_RATE}")))
        );
        // Whitespace around the value and the components is tolerated.
        assert_eq!(
            parse_node_latency(Some(" 1024 / 48000 ")),
            Some(("node.latency", "1024/48000".to_string()))
        );
    }

    #[test]
    fn node_latency_invalid_inputs_are_ignored_not_fatal() {
        for bad in [
            "0", "0/48000", "480/0", "-5", "abc", "480/", "/48000", "4.8",
        ] {
            assert_eq!(parse_node_latency(Some(bad)), None, "{bad} must be ignored");
        }
    }
}
