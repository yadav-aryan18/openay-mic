//! Feature-off error path for the Bluetooth transport: without the
//! `bluetooth` feature, a `Start` on [`Transport::Bluetooth`] must fail
//! *cleanly* — the pipeline starts, the network task immediately reports
//! "built without Bluetooth support", the engine reaper records it in
//! `last_error` and flips `running` back to `false` — and the handle must
//! stay usable for a normal UDP run afterwards (mirrors the bind-conflict
//! recovery contract from `engine_smoke.rs`).
//!
//! Compiled only *without* the `bluetooth` feature (headless
//! `cargo test -p openay-server`); with the feature on, the file is an
//! empty test crate.

#![cfg(not(feature = "bluetooth"))]

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use openay_server::{spawn_engine, CodecMode, EngineCommand, EngineConfig, Transport};

/// Ask the OS for a free UDP port (used only by the recovery half).
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
fn bt_transport_fails_cleanly_without_feature() {
    let config = EngineConfig {
        transport: Transport::Bluetooth,
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        // Still validated and stored (for display) even though the BT loop
        // would never use it.
        port: 41_700,
        codec: CodecMode::Auto,
        target_ms: 10.0,
        capacity_ms: 100.0,
        audio_output: false,
        output_device: None,
        // Unity gain: this test predates the receiver-gain config field.
        gain: 1.0,
    };
    let handle = spawn_engine(Some(config.clone()));

    // Cold-start contract: spawn never runs; no error yet.
    assert!(!handle.status().running);
    assert!(handle.last_error().is_none(), "no error before Start");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(config.clone()))
            .await
            .expect("send Start");
    });

    // The pipeline starts, the (missing) BT loop fails immediately, and the
    // reaper records the error and stops the engine — the same path as a
    // bind failure.
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| {
            !s.running && handle.last_error().is_some()
        }),
        "engine must report the missing-feature failure: running={} last_error={:?}",
        handle.status().running,
        handle.last_error()
    );
    let err = handle.last_error().expect("last_error must be set");
    assert!(
        err.contains("without Bluetooth support"),
        "last_error must name the missing Bluetooth support, got: {err}"
    );

    // Nothing was received (nothing could be), the standby display still
    // shows the bt config, and a failed run leaves no stats line.
    let s = handle.status();
    assert!(!s.running);
    assert_eq!(s.transport, Transport::Bluetooth);
    assert_eq!(s.received, 0);
    assert_eq!(s.lost, 0);
    assert_eq!(s.malformed, 0);
    assert_eq!(
        handle.take_stats_line(),
        None,
        "a failed run must not leave a stats line"
    );

    // The handle stays usable: a normal UDP run works on the same engine.
    let udp_config = EngineConfig {
        transport: Transport::Udp,
        port: free_udp_port(),
        ..config.clone()
    };
    rt.block_on(async {
        handle
            .cmd()
            .send(EngineCommand::Start(udp_config))
            .await
            .expect("send Start");
    });
    assert!(
        wait_until(&handle, Duration::from_secs(5), |s| s.running),
        "engine must recover on the same handle with UDP: status={:?} last_error={:?}",
        handle.status(),
        handle.last_error()
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
        "engine must stop"
    );
}
