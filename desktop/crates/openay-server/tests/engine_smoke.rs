//! Headless smoke test of the engine's NON-GUI logic: construct an
//! [`EngineHandle`], start the UDP engine on a free port, feed 100 packets
//! via [`openay_transport::UdpSender`], and assert the status counters.
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
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must reach the running state"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
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

    // Stop, then confirm the handle is reusable and reports the final stats.
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
    assert!(!stopped.running);
    assert_eq!(
        stopped.received, 100,
        "stopped status keeps the final stats"
    );
    assert_eq!(stopped.lost, 0);
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

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    // Wait for the initial start to actually happen (otherwise "not
    // running" below would be indistinguishable from "not started yet").
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
}
