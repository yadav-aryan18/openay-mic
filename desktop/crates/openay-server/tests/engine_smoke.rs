//! Headless smoke test of the engine's NON-GUI logic: construct an
//! [`EngineHandle`], start the UDP engine on a free port, feed 100 packets
//! via [`openay_transport::UdpSender`], and assert the status counters.
//!
//! The tests exercise the **cold-start contract**: `spawn_engine` never
//! starts a pipeline (the optional config is only defaults for the first
//! `Start`), and a snapshot with `running == false` always reads zeroed
//! counters — the run's final numbers are only available via
//! `take_stats_line`.
//!
//! Level metering is RT-callback driven and PipeWire-only: in this
//! network-only build (`pipewire` feature off) nothing consumes the jitter
//! buffer, so `level_peak` stays `0.0` — asserted and documented here, per
//! the Phase 5 spec.

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use openay_protocol::{Packet, PayloadType};
use openay_server::{
    spawn_engine, CodecMode, EngineCommand, EngineConfig, Transport, MAX_PREBUFFER_MS,
    MIN_PREBUFFER_MS,
};
use openay_transport::UdpSender;

/// A payload of `len` xorshift-filler bytes (matches the interop filler,
/// content is irrelevant to the engine beyond being decodable).
fn pcm_payload(len: usize, seed: u32) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    openay_transport::fill_xorshift(&mut buf, seed);
    buf
}

/// Ask the OS for a free UDP port, then release it (the engine binds it
/// next; good enough for a test, no race in practice).
fn free_udp_port() -> u16 {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral socket");
    socket.local_addr().expect("local addr").port()
}

/// Poll `status` until `pred` holds or the timeout elapses.
fn wait_until<F: Fn(&openay_server::EngineStatus) -> bool>(
    handle: &openay_server::EngineHandle,
    timeout: Duration,
    pred: F,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let s = handle.status();
        if pred(&s) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Assert the full per-run zeroing contract of a stopped/cold snapshot.
fn assert_zeroed(s: &openay_server::EngineStatus) {
    assert!(!s.running, "engine must not be running");
    assert_eq!(s.received, 0, "received must be 0 while stopped");
    assert_eq!(s.lost, 0, "lost must be 0 while stopped");
    assert_eq!(s.dup, 0, "dup must be 0 while stopped");
    assert_eq!(s.ooo, 0, "ooo must be 0 while stopped");
    assert_eq!(s.malformed, 0, "malformed must be 0 while stopped");
    assert_eq!(s.overruns, 0, "overruns must be 0 while stopped");
    assert_eq!(s.underruns, 0, "underruns must be 0 while stopped");
    assert_eq!(s.fill_ms, 0.0, "fill_ms must be 0 while stopped");
    assert_eq!(s.level_peak, 0.0, "level_peak must be 0 while stopped");
    assert_eq!(s.uptime_secs, 0, "uptime_secs must be 0 while stopped");
}

/// A current-thread tokio runtime for driving senders/commands in tests.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

#[test]
fn engine_receives_udp_packets_without_pipewire() {
    let port = free_udp_port();
    let config = EngineConfig {
        transport: Transport::Udp,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        codec: CodecMode::Auto,
        target_ms: 10.0,
        capacity_ms: 100.0,
    }
    .validated()
    .expect("config valid");
    assert!(config.target_ms >= MIN_PREBUFFER_MS && config.target_ms <= MAX_PREBUFFER_MS);

    let handle = spawn_engine(Some(config));

    // Cold-start contract: spawn_engine(Some(config)) must NOT bind or run —
    // the config is only the standby/default. Counters are zero, the config
    // fields are still reported for the standby display.
    let cold = handle.status();
    assert_zeroed(&cold);
    assert_eq!(cold.transport, Transport::Udp);
    assert_eq!(cold.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(
        cold.port, port,
        "standby status reports the configured port"
    );
    assert_eq!(cold.codec, CodecMode::Auto);
    assert_eq!(handle.last_error(), None, "no error before the first Start");

    let rt = rt();

    // Start explicitly.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must reach the running state"
    );

    rt.block_on(async {
        let sender = UdpSender::connect("127.0.0.1", port)
            .await
            .expect("connect sender");

        // Probe: the socket bind happens inside the engine task, so the
        // first packet may be dropped before the listener is up. Resend the
        // probe (seq 0) until one lands, sleeping between probes so at most
        // one datagram is ever in flight (no duplicates); once a probe
        // lands the socket is definitely bound and the remaining 99 packets
        // all arrive in order.
        loop {
            sender
                .send_packet(&Packet {
                    kind: PayloadType::Pcm,
                    seq: 0,
                    payload: pcm_payload(480, 0),
                })
                .await
                .expect("send probe");
            tokio::time::sleep(Duration::from_millis(50)).await;
            if handle.status().received >= 1 {
                break;
            }
        }
        for seq in 1..100u16 {
            sender
                .send_packet(&Packet {
                    kind: PayloadType::Pcm,
                    seq,
                    payload: pcm_payload(480, seq as u32),
                })
                .await
                .expect("send packet");
        }
    });

    // All 100 packets must land, in order, with no loss.
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 100),
        "engine must ingest all 100 packets, got {}",
        handle.status().received
    );

    let live = handle.status();
    assert!(live.running);
    assert_eq!(live.received, 100);
    assert_eq!(live.lost, 0);
    assert_eq!(live.dup, 0);
    assert_eq!(live.ooo, 0);
    assert_eq!(live.malformed, 0);
    // 100 x 480 samples = 48 000 samples = 1 s of audio; the 100 ms
    // (4800 sample) jitter buffer rounds up to 8192 samples (170.67 ms) and
    // is now exactly full: overruns were counted and fill_ms == capacity.
    let capacity_ms = 8192.0 * 1000.0 / openay_server::SAMPLE_RATE as f32;
    assert!(live.overruns > 0, "buffer overflow must be counted");
    assert!(
        (live.fill_ms - capacity_ms).abs() < 0.01,
        "fill_ms={} must equal the full-buffer capacity {capacity_ms}",
        live.fill_ms
    );
    // No PipeWire: nothing consumes the buffer, so the level stays 0.0.
    // (With the `pipewire` feature the RT callback would fold max |sample|
    // into this field — documented, and asserted as 0.0 here on purpose.)
    assert_eq!(
        live.level_peak, 0.0,
        "without the pipewire feature no audio is consumed, level stays 0.0"
    );

    // Stop, then confirm the snapshot is zeroed and the handle is reusable;
    // the run's final numbers survive only in the stats line.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Stop)
            .await
            .expect("send Stop");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| !s.running),
        "engine must reach the stopped state"
    );
    let stopped = handle.status();
    assert_zeroed(&stopped);
    // The canonical stats line is available to the CLI wrapper.
    let line = handle.take_stats_line().expect("final stats line");
    assert!(
        line.starts_with("SRV transport=udp received=100 lost=0 dup=0 ooo=0 malformed=0"),
        "{line}"
    );
}

/// The engine must survive a Stop -> Start cycle on the same handle with a
/// different config (the GUI's settings-change path).
#[test]
fn engine_restarts_on_same_handle() {
    let port = free_udp_port();
    let config = EngineConfig {
        transport: Transport::Udp,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        codec: CodecMode::Auto,
        target_ms: 10.0,
        capacity_ms: 100.0,
    };
    let handle = spawn_engine(Some(config));

    // Cold-start contract: the engine is idle even though a config was given.
    assert!(!handle.status().running, "engine must be cold after spawn");

    let rt = rt();

    // Start explicitly.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must reach the running state"
    );

    // Stop.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Stop)
            .await
            .expect("send Stop");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| !s.running),
        "engine must stop"
    );
    assert_zeroed(&handle.status());

    // Restart with a different codec mode and target; same handle.
    let config2 = EngineConfig {
        codec: CodecMode::Pcm,
        target_ms: 20.0,
        ..config
    };
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config2))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| {
            s.running && s.codec == CodecMode::Pcm
        }),
        "engine must run again on the same handle with the new codec, got {:?}",
        handle.status()
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
        "engine must stop again"
    );
    assert_zeroed(&handle.status());
}

/// Defect 3: a `Start` whose port is already bound (the test holds the
/// socket) must end with `running == false` and a non-empty `last_error`,
/// and a later `Start` on a free port must succeed on the *same* handle.
#[test]
fn bind_conflict_reports_error_and_recovers() {
    // Hold a UDP socket so the engine's bind must fail.
    let holder = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind holder");
    let port = holder.local_addr().expect("holder local addr").port();
    let config = EngineConfig {
        transport: Transport::Udp,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        codec: CodecMode::Auto,
        target_ms: 10.0,
        capacity_ms: 100.0,
    };
    let handle = spawn_engine(Some(config));
    // Cold even with a config on a *taken* port: spawn never binds.
    assert!(
        !handle.status().running,
        "engine must be cold after spawn even with a config on a held port"
    );

    let rt = rt();

    // Start on the conflicted port.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| {
            !s.running && handle.last_error().is_some()
        }),
        "engine must report the bind failure: running={} last_error={:?}",
        handle.status().running,
        handle.last_error()
    );
    let err = handle.last_error().expect("last_error must be set");
    assert!(
        err.contains("bind") || err.contains("address"),
        "last_error must explain the bind failure, got: {err}"
    );
    assert_zeroed(&handle.status());
    assert_eq!(
        handle.take_stats_line(),
        None,
        "a failed run must not leave a stats line"
    );

    // Free the port and start again on the same handle.
    drop(holder);
    let port2 = free_udp_port();
    let config2 = EngineConfig {
        port: port2,
        ..config
    };
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config2))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must recover on a free port: status={:?} last_error={:?}",
        handle.status(),
        handle.last_error()
    );
    assert_eq!(
        handle.last_error(),
        None,
        "a successful Start clears the previous error"
    );

    // Prove the recovered pipeline actually receives.
    rt.block_on(async {
        let sender = UdpSender::connect("127.0.0.1", port2)
            .await
            .expect("connect sender");
        for seq in 0..5u16 {
            sender
                .send_packet(&Packet {
                    kind: PayloadType::Pcm,
                    seq,
                    payload: pcm_payload(480, seq as u32),
                })
                .await
                .expect("send packet");
        }
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 5),
        "recovered pipeline must receive packets, got {}",
        handle.status().received
    );

    // Clean stop.
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Stop)
            .await
            .expect("send Stop");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| !s.running),
        "engine must stop"
    );
    assert_zeroed(&handle.status());
}

/// The engine must survive rapid Start/Stop churn on one handle without
/// panicking or leaking OS threads. The engine thread and its tokio runtime
/// workers persist by design (the handle stays reusable), so the assertion
/// is a *bounded* growth check against the post-warmup baseline rather than
/// an absolute count.
#[test]
fn rapid_start_stop_churn_does_not_leak_threads() {
    let port = free_udp_port();
    let config = EngineConfig {
        transport: Transport::Udp,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        codec: CodecMode::Auto,
        target_ms: 10.0,
        capacity_ms: 100.0,
    };
    let handle = spawn_engine(Some(config));
    assert!(!handle.status().running, "engine must be cold after spawn");

    let rt = rt();
    let start = || {
        rt.block_on(async {
            handle
                .cmd()
                .send(EngineCommand::Start(config))
                .await
                .expect("send Start");
        })
    };
    let stop = || {
        rt.block_on(async {
            handle
                .cmd()
                .send(EngineCommand::Stop)
                .await
                .expect("send Stop");
        })
    };

    // Warmup cycle: lets the tokio runtime lazily spawn its worker/blocking
    // threads before we take the baseline.
    start();
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "warmup start must run"
    );
    stop();
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| !s.running),
        "warmup stop must stop"
    );

    let baseline = thread_count();
    const CHURN: u32 = 10;
    for i in 0..CHURN {
        start();
        assert!(
            wait_until(&handle, Duration::from_secs(5), |s| s.running),
            "churn cycle {i}: engine must run"
        );
        stop();
        assert!(
            wait_until(&handle, Duration::from_secs(5), |s| !s.running),
            "churn cycle {i}: engine must stop"
        );
    }

    // Let any lazy-spawned helper threads settle before counting.
    std::thread::sleep(Duration::from_millis(500));
    let after = thread_count();

    if let (Some(b), Some(a)) = (baseline, after) {
        // The engine thread + tokio workers persist across churn (by design,
        // the handle is reusable); allow a small fudge for lazy helper
        // threads. A leak would show as linear growth (several threads per
        // churn cycle).
        assert!(
            a <= b + 3,
            "thread count grew from {b} to {a} after {CHURN} Start/Stop cycles — likely a thread leak"
        );
    }
    // On systems without /proc/self/status the thread assertion is skipped
    // (the functional churn checks above still ran).
}

/// Number of OS threads in this process, from `/proc/self/status`
/// (`None` when unavailable, e.g. non-Linux).
fn thread_count() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| {
        l.strip_prefix("Threads:")
            .and_then(|v| v.trim().parse().ok())
    })
}
