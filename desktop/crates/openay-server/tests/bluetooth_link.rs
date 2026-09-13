//! End-to-end test of the Bluetooth (SPP) receive path — over an **injected
//! in-process connection source**, no Bluetooth hardware or BlueZ involved.
//!
//! The engine is started on [`Transport::Bluetooth`] via
//! [`spawn_engine_with_bt_source`], which swaps the BlueZ SPP server for an
//! [`InjectedBtSource`]. Each `push`ed duplex byte stream is served by the
//! **exact** production connection loop used for accepted RFCOMM connections
//! (`stream_accept_loop` + `serve_packet_stream`): the same wire framing
//! (with bad-magic resync), the same ingest path, the same per-connection
//! task structure as TCP. The test writes real wire-framed PCM packets
//! (`openay_protocol::encode`, 6-byte header + payload) through the phone
//! side of a `tokio::io::duplex` pair and asserts the engine's status
//! counters and the canonical `SRV transport=bt` stats line.
//!
//! Two sequential "phone connections" are pushed: the normal case is one
//! connection at a time, but the engine must keep accepting reconnects.
//!
//! Compiled only with the `bluetooth` feature (a plain `cargo test
//! -p openay-server` compiles it out; `cargo test --workspace` enables the
//! feature via openay-gui).

#![cfg(feature = "bluetooth")]

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use openay_jitter::DepthParams;
use openay_protocol::{encode, Packet, PayloadType};
use openay_server::{
    spawn_engine_with_bt_source, BoxedByteStream, CodecMode, EngineCommand, EngineConfig,
    InjectedBtSource, Transport,
};
use tokio::io::AsyncWriteExt;

/// A payload of `len` xorshift-filler bytes (content is irrelevant to the
/// engine beyond being decodable PCM).
fn pcm_payload(len: usize, seed: u32) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    openay_transport::fill_xorshift(&mut buf, seed);
    buf
}

/// A current-thread tokio runtime for driving the phone side and commands.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
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

/// Encode a PCM packet's wire bytes (6-byte header + payload).
fn wire_frame(seq: u16) -> Vec<u8> {
    encode(&Packet {
        kind: PayloadType::Pcm,
        seq,
        payload: pcm_payload(480, seq as u32),
    })
}

/// The full BT link: start the engine on `Transport::Bluetooth` with an
/// injected source, stream 50 framed packets through one duplex connection,
/// reconnect (a second connection continuing the sequence), stream 50 more,
/// and verify the status counters, the connected-device slot, and the final
/// `SRV transport=bt ...` stats line. Also exercises bad-magic resync by
/// prefixing the first connection's byte stream with non-OpenAY garbage.
#[test]
fn bt_injected_stream_end_to_end() {
    let inject = InjectedBtSource::default();
    let config = EngineConfig {
        transport: Transport::Bluetooth,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        // Ignored by the BT loop (SPP listens via the SDP record), but kept
        // valid and stored for display.
        port: 41_700,
        codec: CodecMode::Auto,
        target_ms: 10.0,
        capacity_ms: 100.0,
    };
    let handle =
        spawn_engine_with_bt_source(Some(config), DepthParams::default(), Some(inject.clone()));

    // Cold-start contract: no pipeline until Start, standby display reports
    // the configured transport.
    let cold = handle.status();
    assert!(!cold.running);
    assert_eq!(cold.transport, Transport::Bluetooth);
    assert!(handle.last_error().is_none(), "no error before Start");

    let rt = rt();
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

    // ---- Connection 1: 10 garbage bytes (no magic) + packets 0..50 ----
    let (engine_end, mut phone) = tokio::io::duplex(64 * 1024);
    inject.push(BoxedByteStream::new(engine_end), "AA:BB:CC:00:00:01");

    rt.block_on(async {
        // Bad-magic garbage: the framing layer must resync onto the first
        // real packet (same behavior as the TCP path).
        phone
            .write_all(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09])
            .await
            .expect("write garbage");
        for seq in 0..50u16 {
            phone
                .write_all(&wire_frame(seq))
                .await
                .expect("write frame");
        }
        phone.flush().await.expect("flush");
    });

    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 50),
        "engine must ingest the first 50 packets, got {} (malformed {})",
        handle.status().received,
        handle.status().malformed
    );
    assert_eq!(
        handle.bt_device().as_deref(),
        Some("AA:BB:CC:00:00:01"),
        "the connected phone's address must surface on the handle"
    );

    // ---- Connection 2: phone reconnects, continues at seq 50 ----
    drop(phone); // phone 1 walks away; its serve task ends on EOF
    let (engine_end2, mut phone2) = tokio::io::duplex(64 * 1024);
    inject.push(BoxedByteStream::new(engine_end2), "AA:BB:CC:00:00:01");
    rt.block_on(async {
        for seq in 50..100u16 {
            phone2
                .write_all(&wire_frame(seq))
                .await
                .expect("write frame");
        }
        phone2.flush().await.expect("flush");
    });

    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 100),
        "engine must ingest all 100 packets across the reconnect, got {}",
        handle.status().received
    );

    let live = handle.status();
    assert!(live.running);
    assert_eq!(live.received, 100);
    assert_eq!(live.lost, 0, "sequential seq 0..100 => no loss");
    assert_eq!(live.dup, 0);
    assert_eq!(live.ooo, 0);
    assert_eq!(
        live.malformed, 0,
        "resynced garbage is discarded, not counted malformed"
    );
    assert_eq!(live.transport, Transport::Bluetooth);

    // ---- Stop: per-run snapshot zeroed, final numbers in the SRV line ----
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
    let stopped = handle.status();
    assert!(!stopped.running);
    assert_eq!(stopped.received, 0, "snapshot counters are per-run");
    assert_eq!(
        handle.bt_device(),
        None,
        "the device slot is cleared when the run stops"
    );
    let line = handle
        .take_stats_line()
        .expect("a completed run leaves its stats line");
    assert!(
        line.starts_with("SRV transport=bt received=100 lost=0 dup=0 ooo=0 malformed=0"),
        "SRV line must carry the bt transport and counters, got: {line}"
    );
}

/// The device slot tracks the *current* connection: while a connection is
/// being served it holds that phone's label, and it clears again when the
/// connection ends (engine keeps running, waiting for a reconnect).
#[test]
fn bt_device_slot_follows_the_connection_lifecycle() {
    let inject = InjectedBtSource::default();
    let config = EngineConfig {
        transport: Transport::Bluetooth,
        ..EngineConfig::default()
    };
    let handle =
        spawn_engine_with_bt_source(Some(config), DepthParams::default(), Some(inject.clone()));
    let rt = rt();
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must run"
    );
    // Waiting for a paired device: no slot content yet.
    assert_eq!(handle.bt_device(), None, "no device before accept");

    let (engine_end, mut phone) = tokio::io::duplex(16 * 1024);
    inject.push(BoxedByteStream::new(engine_end), "DE:AD:BE:EF:00:42");
    rt.block_on(async {
        phone
            .write_all(&wire_frame(0))
            .await
            .expect("write one frame");
        phone.flush().await.expect("flush");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 1),
        "the single packet must land"
    );
    assert_eq!(handle.bt_device().as_deref(), Some("DE:AD:BE:EF:00:42"));

    // Connection ends (phone drops): slot clears while the engine keeps
    // running and accepting.
    drop(phone);
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.received >= 1
            && handle.bt_device().is_none()),
        "the device slot must clear when the connection ends"
    );
    assert!(handle.status().running, "engine keeps listening");

    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Stop)
            .await
            .expect("send Stop");
    });
}
