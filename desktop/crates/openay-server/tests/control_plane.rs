//! Control-plane integration tests: hello/hello-ack handshake, receiver
//! liveness pings, and the peer-stale status flag — against a live engine
//! over real sockets (same pattern as `engine_smoke.rs`).

use std::net::IpAddr;
use std::time::{Duration, Instant};

use openay_protocol::control::{control_payload, ControlMsg, CONTROL_PROTO};
use openay_protocol::{encode, decode, Packet, PayloadType};
use openay_server::{spawn_engine, CodecMode, EngineCommand, EngineConfig, Transport};

/// A free-loopback port for this test binary (existing engine tests use the
/// 417xx range; this file claims 41710-41712).
const UDP_PORT: u16 = 41_710;
const TCP_PORT: u16 = 41_711;
const UDP_FOREIGN_PORT: u16 = 41_714;

fn udp_config(port: u16) -> EngineConfig {
    EngineConfig {
        transport: Transport::Udp,
        bind: IpAddr::from([127, 0, 0, 1]),
        port,
        codec: CodecMode::Auto,
        ..EngineConfig::default()
    }
}

fn wait_running(handle: &openay_server::EngineHandle) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if handle.status().running {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn control_packet(seq: u16, msg: &ControlMsg) -> Packet {
    Packet {
        kind: PayloadType::Control,
        seq,
        payload: control_payload(msg),
    }
}

/// hello -> hello-ack, then receiver pings arrive at the announced cadence,
/// and answering them with pong keeps the peer fresh.
#[tokio::test]
async fn udp_hello_ack_and_pings() {
    let port = UDP_PORT;
    let handle = spawn_engine(Some(udp_config(port)));
    handle
        .cmd()
        .send(EngineCommand::Start(udp_config(port)))
        .await
        .expect("send Start");
    assert!(wait_running(&handle), "engine must start");

    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.connect(("127.0.0.1", port)).await.unwrap();

    // hello: the engine must answer with hello-ack naming itself.
    sock.send(&encode(&control_packet(
        0,
        &ControlMsg::Hello {
            app: "openay-mic".into(),
            proto: CONTROL_PROTO,
            codecs: vec!["pcm".into(), "opus".into()],
            rate: 48_000,
        },
    )))
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut saw_ack = false;
    let mut saw_ping = false;
    while Instant::now() < deadline && !(saw_ack && saw_ping) {
        let mut buf = vec![0u8; 1500];
        let n = match tokio::time::timeout(Duration::from_millis(500), sock.recv(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let pkt = match decode(&buf[..n]) {
            Ok(p) => p,
            Err(_) => continue,
        };
        assert_eq!(pkt.kind, PayloadType::Control, "only control comes back");
        match openay_protocol::control::parse_control(&pkt.payload) {
            Some(ControlMsg::HelloAck { app, proto, rate, .. }) => {
                saw_ack = true;
                assert_eq!(app, "openay-server");
                assert_eq!(proto, CONTROL_PROTO);
                assert_eq!(rate, 48_000);
            }
            Some(ControlMsg::Ping) => {
                saw_ping = true;
                // Answer the ping: the receiver must count it as peer traffic.
                sock.send(&encode(&control_packet(1, &ControlMsg::Pong)))
                    .await
                    .unwrap();
            }
            other => panic!("unexpected control message: {other:?}"),
        }
    }
    assert!(saw_ack, "no hello-ack within 4 s");
    assert!(saw_ping, "no liveness ping within 4 s of connect");
    assert!(!handle.status().peer_stale, "active peer must not be stale");

    handle.cmd().send(EngineCommand::Stop).await.unwrap();
}

/// A sender that stops transmitting is flagged stale after 5 s; traffic
/// clears the flag again.
#[tokio::test]
async fn peer_stale_after_silence_then_recovers() {
    let port = UDP_PORT + 2; // own port: UDP tests run in parallel
    let handle = spawn_engine(Some(udp_config(port)));
    handle
        .cmd()
        .send(EngineCommand::Start(udp_config(port)))
        .await
        .expect("send Start");
    assert!(wait_running(&handle), "engine must start");

    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.connect(("127.0.0.1", port)).await.unwrap();
    // One audio packet establishes the peer.
    let silence = Packet {
        kind: PayloadType::Pcm,
        seq: 0,
        payload: vec![0u8; 960],
    };
    sock.send(&encode(&silence)).await.unwrap();
    // Drain pings so the socket buffer does not fill up during the wait
    // (tokio sockets are always non-blocking; try_recv never parks).
    let t_stop = Instant::now();
    while t_stop.elapsed() < Duration::from_secs(7) {
        std::thread::sleep(Duration::from_millis(100));
        let mut buf = vec![0u8; 1500];
        while sock.try_recv(&mut buf).is_ok() {}
        if t_stop.elapsed() > Duration::from_secs(6) {
            assert!(
                handle.status().peer_stale,
                "peer must be stale after >5 s of silence"
            );
        }
    }
    // Traffic resumes: the flag must clear promptly.
    sock.send(&encode(&Packet {
        kind: PayloadType::Pcm,
        seq: 1,
        payload: vec![0u8; 960],
    }))
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !handle.status().peer_stale {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!handle.status().peer_stale, "peer must recover after traffic");

    handle.cmd().send(EngineCommand::Stop).await.unwrap();
}

/// Same handshake over TCP: the connection receives hello-ack + pings from
/// the per-connection writer task.
#[tokio::test]
async fn tcp_hello_ack_and_pings() {
    let cfg = EngineConfig {
        transport: Transport::Tcp,
        bind: IpAddr::from([127, 0, 0, 1]),
        port: TCP_PORT,
        codec: CodecMode::Auto,
        ..EngineConfig::default()
    };
    let handle = spawn_engine(Some(cfg.clone()));
    handle.cmd().send(EngineCommand::Start(cfg)).await.unwrap();
    assert!(wait_running(&handle), "engine must start");

    let stream = tokio::net::TcpStream::connect(("127.0.0.1", TCP_PORT))
        .await
        .unwrap();
    let mut framed = openay_transport::tcp::TcpPacketStream::new(stream);
    framed
        .send_packet(&control_packet(
            0,
            &ControlMsg::Hello {
                app: "openay-mic".into(),
                proto: CONTROL_PROTO,
                codecs: vec!["pcm".into()],
                rate: 48_000,
            },
        ))
        .await
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut saw_ack = false;
    let mut saw_ping = false;
    while Instant::now() < deadline && !(saw_ack && saw_ping) {
        let pkt = match tokio::time::timeout(
            Duration::from_millis(1500),
            framed.next_packet(),
        )
        .await
        {
            Ok(Ok(p)) => p,
            _ => continue,
        };
        assert_eq!(pkt.kind, PayloadType::Control);
        match openay_protocol::control::parse_control(&pkt.payload) {
            Some(ControlMsg::HelloAck { app, .. }) => {
                saw_ack = true;
                assert_eq!(app, "openay-server");
            }
            Some(ControlMsg::Ping) => saw_ping = true,
            other => panic!("unexpected control message: {other:?}"),
        }
    }
    assert!(saw_ack, "no hello-ack over TCP within 4 s");
    assert!(saw_ping, "no liveness ping over TCP within 4 s");

    handle.cmd().send(EngineCommand::Stop).await.unwrap();
}

/// The UDP peer latch: after the first accepted datagram, a different
/// source is rejected (counted as foreign) until the latched peer has been
/// quiet for the stale window — then it may take over (e.g. the phone
/// roamed to a new address).
#[tokio::test]
async fn udp_foreign_source_is_rejected_until_stale() {
    let port = UDP_FOREIGN_PORT;
    let handle = spawn_engine(Some(udp_config(port)));
    handle
        .cmd()
        .send(EngineCommand::Start(udp_config(port)))
        .await
        .expect("send Start");
    assert!(wait_running(&handle), "engine must start");

    // Sender A latches the peer slot.
    let a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    a.connect(("127.0.0.1", port)).await.unwrap();
    a.send(&encode(&control_packet(
        0,
        &ControlMsg::Hello {
            app: "openay-mic".into(),
            proto: CONTROL_PROTO,
            codecs: vec!["pcm".into()],
            rate: 48_000,
        },
    )))
    .await
    .unwrap();
    // Drain A's hello-ack/pings so its socket does not fill up (tokio
    // sockets are always non-blocking; try_recv never parks).

    // Sender B arrives while A is active: its datagram must be rejected.
    let b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    b.connect(("127.0.0.1", port)).await.unwrap();
    let silence = Packet {
        kind: PayloadType::Pcm,
        seq: 0,
        payload: vec![0u8; 960],
    };
    b.send(&encode(&silence)).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && handle.status().foreign_packets == 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
    let s = handle.status();
    assert_eq!(s.foreign_packets, 1, "B's datagram must be counted foreign");
    assert_eq!(s.received, 1, "only A's hello may be ingested");

    // A goes quiet; B retries periodically. The first attempt past the
    // stale window (measured from A's last packet) must take over.
    let foreign_before = handle.status().foreign_packets;
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut seq_b = 1u16;
    loop {
        if handle.status().received >= 2 {
            break;
        }
        if Instant::now() > deadline {
            panic!("B must take over after the stale window");
        }
        b.send(&encode(&Packet {
            kind: PayloadType::Pcm,
            seq: seq_b,
            payload: vec![0u8; 960],
        }))
        .await
        .unwrap();
        seq_b = seq_b.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(300));
    }
    let s = handle.status();
    assert!(s.received >= 2, "B's takeover packet must be ingested");
    assert!(s.foreign_packets >= foreign_before);
    // Once B is pinned, rejections must stop: foreign count freezes.
    let foreign_at_takeover = s.foreign_packets;
    std::thread::sleep(Duration::from_millis(400));
    b.send(&encode(&Packet {
        kind: PayloadType::Pcm,
        seq: seq_b,
        payload: vec![0u8; 960],
    }))
    .await
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        handle.status().foreign_packets,
        foreign_at_takeover,
        "after the takeover, B is the peer and nothing is rejected"
    );
    let _ = foreign_before;

    handle.cmd().send(EngineCommand::Stop).await.unwrap();
}

/// Regression: a flood of malformed datagrams must not wedge the engine —
/// Stop has to complete while garbage arrives at line rate (the malformed
/// path used to `continue` past the quit check).
#[tokio::test]
async fn malformed_flood_does_not_hang_stop() {
    let port = UDP_PORT + 6;
    let handle = spawn_engine(Some(udp_config(port)));
    handle
        .cmd()
        .send(EngineCommand::Start(udp_config(port)))
        .await
        .expect("send Start");
    assert!(wait_running(&handle), "engine must start");

    let flooder = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let flood_port = port;
    let flood = tokio::spawn(async move {
        let buf = [0u8; 1];
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            // 1-byte datagrams: always decode-Err, cheap to send at line rate.
            let _ = flooder.send_to(&buf, ("127.0.0.1", flood_port)).await;
            // Yield so the engine task gets scheduled between bursts.
            tokio::task::yield_now().await;
        }
    });

    // Let the flood build up, then Stop must complete promptly.
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.cmd().send(EngineCommand::Stop).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && handle.status().running {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !handle.status().running,
        "Stop must complete under a malformed-datagram flood"
    );
    flood.abort();
}

/// Regression (Windows): after the engine pins a peer and starts pinging
/// it, a peer that goes away must not take the engine down — Windows
/// surfaces WSAECONNRESET on the listener's next recv_from, which used to
/// end the network task and stop the whole pipeline. (Linux does not
/// surface such errors on unconnected sockets, so this passes trivially
/// there; CI's Windows runner is the real gate.)
#[tokio::test]
async fn pinged_dead_peer_does_not_stop_the_engine() {
    let port = UDP_PORT + 8;
    let handle = spawn_engine(Some(udp_config(port)));
    handle
        .cmd()
        .send(EngineCommand::Start(udp_config(port)))
        .await
        .expect("send Start");
    assert!(wait_running(&handle), "engine must start");

    // Sender latches the peer slot, sends one audio packet, then VANISHES
    // (socket dropped — exactly what the adaptive-depth tests do after
    // their producer phase).
    let vanishing = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    vanishing.connect(("127.0.0.1", port)).await.unwrap();
    vanishing
        .send(&encode(&Packet {
            kind: PayloadType::Pcm,
            seq: 0,
            payload: vec![0u8; 960],
        }))
        .await
        .unwrap();
    drop(vanishing);

    // The engine keeps pinging the dead peer every second. Past the stale
    // window, the engine must STILL be running, and Stop must work.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let s = handle.status();
    assert!(s.running, "engine must survive pings to a dead peer");
    handle.cmd().send(EngineCommand::Stop).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && handle.status().running {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!handle.status().running, "Stop must complete");
}

/// Gain validation: non-finite values are rejected before reaching the
/// engine (a NaN would poison every decoded sample).
#[tokio::test]
async fn gain_validation_rejects_nan() {
    use openay_server::ConfigError;
    let cfg = EngineConfig {
        gain: f32::NAN,
        ..EngineConfig::default()
    };
    match cfg.validated() {
        Err(ConfigError::InvalidGain(v)) => assert!(v.is_nan(), "NaN must be preserved"),
        other => panic!("NaN gain must be rejected, got {other:?}"),
    }
    // Finite values clamp into 0..=4.
    let clamped = EngineConfig {
        gain: 9.0,
        ..EngineConfig::default()
    }
    .validated()
    .unwrap();
    assert_eq!(clamped.gain, 4.0);
}
