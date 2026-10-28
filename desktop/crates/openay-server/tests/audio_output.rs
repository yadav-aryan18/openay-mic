//! Integration tests for the `audio-output` engine path (feature-gated).
//!
//! - The headless test exercises the failure + recovery contract: a Start
//!   whose `output_device` matches no device must fail cleanly (the engine
//!   reports `running == false` with `last_error` set), and the same handle
//!   must accept a later Start with a good config.
//! - The live test (default `#[ignore]`: needs a working audio output
//!   device — on this host the PipeWire ALSA interface) streams ~3 s of real
//!   framed PCM (440 Hz sine, 480-sample 10 ms frames over UDP, the phone's
//!   wire format) into an engine with `audio_output: true` and asserts the
//!   packet counters and a zero-underrun run.

#![cfg(feature = "audio-output")]

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use openay_protocol::{encode, Packet, PayloadType};
use openay_server::{spawn_engine, EngineCommand, EngineConfig};

/// A current-thread tokio runtime for driving the handle's async commands.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

/// Poll `status()` until `pred` holds or the timeout elapses.
fn wait_until(
    handle: &openay_server::EngineHandle,
    timeout: Duration,
    mut pred: impl FnMut(&openay_server::EngineStatus) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred(&handle.status()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Test-environment fixup for hosts where the ambient libasound cannot find
/// the host's PCM plugins (here: the conda libasound looks in its own prefix
/// and misses the system PipeWire glue plugin, so ALSA's "default" device
/// fails with ENODEV). When `ALSA_PLUGIN_DIR` is unset, probe the standard
/// system plugin directories for the PipeWire glue and point ALSA at it.
/// Production setups configure this via the environment; only this live
/// test needs the workaround.
fn fixup_alsa_plugin_dir() {
    if std::env::var_os("ALSA_PLUGIN_DIR").is_some() {
        return;
    }
    const CANDIDATES: [&str; 4] = [
        "/usr/lib/x86_64-linux-gnu/alsa-lib",
        "/usr/lib64/alsa-lib",
        "/usr/lib/alsa-lib",
        "/usr/local/lib/alsa-lib",
    ];
    let dir = CANDIDATES.iter().map(std::path::Path::new).find(|d| {
        d.join("libasound_module_pcm_pipewire.so").is_file()
    });
    if let Some(dir) = dir {
        std::env::set_var("ALSA_PLUGIN_DIR", dir);
        eprintln!("openay-server tests: ALSA_PLUGIN_DIR={dir:?}");
    }
}

/// A fresh engine config on the loopback port: UDP, auto codec, default
/// depths, audio output per the arguments. Built per call (with the feature
/// on, `EngineConfig` is not `Copy`).
fn config(port: u16, audio_output: bool, output_device: Option<&str>) -> EngineConfig {
    EngineConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        audio_output,
        output_device: output_device.map(str::to_string),
        ..EngineConfig::default()
    }
}

/// A Start with an output-device substring that matches nothing must fail
/// cleanly (engine stops, `last_error` explains it), and the same handle
/// must start a normal (headless) run afterwards.
#[test]
fn no_matching_device_fails_cleanly_and_handle_is_reusable() {
    let port = 41_810;
    let handle = spawn_engine(None);
    let rt = rt();

    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config(
                port,
                true,
                Some("zzz-openay-no-such-device-12938"),
            )))
            .await
            .expect("send Start");
    });

    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| {
            !s.running && handle.last_error().is_some()
        }),
        "a Start with a nonexistent output device must fail cleanly, \
         status={:?} last_error={:?}",
        handle.status(),
        handle.last_error()
    );
    let err = handle.last_error().expect("last_error must be set");
    assert!(
        err.contains("audio output"),
        "the error must name the audio output subsystem, got: {err}"
    );
    // (Unlike a bind failure — where run_network returns Err — a device
    // failure lets the network task end normally, so the run's stats line
    // exists; the error is what the GUI/CLI surface.)

    // The handle is reusable: a Start with a good config must run (and
    // clear the previous error).
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config(port, false, None)))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must start on the same handle after the failed run"
    );
    assert_eq!(
        handle.last_error(),
        None,
        "a successful Start clears the previous error"
    );

    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Stop)
            .await
            .expect("send Stop");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| !s.running),
        "engine must stop cleanly"
    );
}

/// LIVE: stream ~3 s of real 440 Hz PCM (300 x 10 ms frames) over UDP into
/// an engine playing on the DEFAULT output device; assert every packet
/// arrived, the run stayed underrun-free, and the stop is clean.
///
/// `#[ignore]` by default: requires an audio output device on the host
/// (run with `cargo test -p openay-server --features audio-output
/// -- --ignored`). On this host the device is PipeWire's ALSA interface.
#[test]
#[ignore = "live: requires a working audio output device (PipeWire ALSA on this host)"]
fn live_audio_output_streams_tone_without_underruns() {
    fixup_alsa_plugin_dir();
    // Bail out with a clear message when the host has no output device at
    // all (rather than a confusing setup failure).
    let devices = openay_server::list_output_devices();
    assert!(
        !devices.is_empty(),
        "live test requires an audio output device; host reports none"
    );
    eprintln!("openay-server tests: output devices = {devices:?}");

    let port = 41_811;
    let handle = spawn_engine(None);
    let rt = rt();

    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config(port, true, None)))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must reach the running state"
    );

    // The phone-side stand-in: 300 x 480-sample i16-LE PCM frames (440 Hz at
    // 0.4 FS — the C++ TestSource amplitude), paced at one 10 ms frame per
    // interval: exactly the realtime pace of the real stream.
    let sender = std::thread::Builder::new()
        .name("tone-sender".into())
        .spawn(move || {
            let sock = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .expect("bind sender socket");
            sock.connect((Ipv4Addr::LOCALHOST, port))
                .expect("connect sender");
            for seq in 0..300u16 {
                let mut payload = Vec::with_capacity(480 * 2);
                for i in 0..480u16 {
                    let t = (u32::from(seq) * 480 + u32::from(i)) as f32 / 48_000.0;
                    let s = (0.4 * 32_767.0 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()) as i16;
                    payload.extend_from_slice(&s.to_le_bytes());
                }
                let wire = encode(&Packet {
                    kind: PayloadType::Pcm,
                    seq,
                    payload,
                });
                sock.send(&wire).expect("send frame");
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .expect("spawn tone sender");

    // Snapshot WHILE the stream is still flowing (frames 290..300 in
    // flight): this is the meaningful underrun window. A stream that ends
    // while the RT callback keeps pulling always produces exactly one dry
    // pop ~20 ms after the last frame (the latch drains to true silence —
    // the RT contract's latch reset), so underruns are asserted mid-stream
    // and NOT on the final stats line.
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 290),
        "engine must receive ~300 packets, got {}",
        handle.status().received
    );
    let s = handle.status();
    assert!(s.running, "engine must still be running after the stream");
    assert!(
        (285..=300).contains(&s.received),
        "received must be ~300, got {}",
        s.received
    );
    assert_eq!(s.lost, 0);
    assert_eq!(s.malformed, 0);
    assert_eq!(
        s.underruns, 0,
        "a realtime-paced stream must not underrun while audio flows \
         (target_ms={}, fill_ms={})",
        s.effective_target_ms, s.fill_ms
    );

    // The sender runs ~3 s in realtime; joining waits for the whole stream.
    sender.join().expect("tone sender must not panic");
    // Give the UDP task a beat to ingest the final datagrams.
    assert!(
        wait_until(&handle, Duration::from_secs(2), |s| s.received == 300),
        "engine must ingest all 300 packets, got {}",
        handle.status().received
    );

    // Clean stop on the same handle.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Stop)
            .await
            .expect("send Stop");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| !s.running),
        "engine must stop cleanly after the live run"
    );
    // The canonical stats line carries the run's final numbers (underruns
    // may read 1 here from the post-stream drain — see the note above).
    let line = handle.take_stats_line().expect("final stats line");
    eprintln!("openay-server tests: {line}");
    assert!(
        line.contains("received=300"),
        "stats line must show 300 received, got: {line}"
    );
}
