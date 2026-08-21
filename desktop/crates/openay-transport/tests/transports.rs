//! Integration tests for the UDP and TCP transports.

use std::time::Duration;

use openay_protocol::{encode, Packet, PayloadType};
use openay_transport::{fill_xorshift, TcpPacketStream, UdpReceiver, UdpSender};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Generate the `n`-th test packet (alternating Pcm/Opus, xorshift payload
/// seeded with the sequence number) exactly as `openay-loopback` does.
fn gen_packet(seq: u16, payload_size: usize) -> Packet {
    let kind = if seq.is_multiple_of(2) {
        PayloadType::Pcm
    } else {
        PayloadType::Opus
    };
    let mut payload = vec![0u8; payload_size];
    fill_xorshift(&mut payload, seq as u32);
    Packet { kind, seq, payload }
}

fn assert_packet_matches(pkt: &Packet, seq: u16, payload_size: usize) {
    let expected_kind = if seq.is_multiple_of(2) {
        PayloadType::Pcm
    } else {
        PayloadType::Opus
    };
    assert_eq!(pkt.kind, expected_kind, "kind mismatch at seq {seq}");
    assert_eq!(pkt.seq, seq, "seq mismatch");
    assert_eq!(pkt.payload.len(), payload_size, "payload length mismatch");
    let mut expect = vec![0u8; payload_size];
    fill_xorshift(&mut expect, seq as u32);
    assert_eq!(pkt.payload, expect, "payload content mismatch at seq {seq}");
}

const COUNT: u16 = 1000;
const PAYLOAD_SIZE: usize = 480;

#[tokio::test]
async fn udp_loopback_1000_packets() {
    let mut rx = UdpReceiver::bind(0).await.unwrap();
    // Larger kernel buffer so a flat-out sender does not overflow the queue.
    let _ = rx.set_recv_buffer_size(4 * 1024 * 1024);
    let port = rx.local_port().unwrap();
    let tx = UdpSender::connect("127.0.0.1", port).await.unwrap();

    // Send and receive concurrently: the sender self-paces on kernel
    // backpressure (ENOBUFS) once the receive queue fills, so nothing drops.
    let sender = tokio::spawn(async move {
        for seq in 0..COUNT {
            tx.send_packet(&gen_packet(seq, PAYLOAD_SIZE))
                .await
                .unwrap();
        }
    });

    for seq in 0..COUNT {
        let pkt = rx
            .recv_packet(Duration::from_secs(5))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("timed out waiting for seq {seq}"));
        assert_packet_matches(&pkt, seq, PAYLOAD_SIZE);
    }
    sender.await.unwrap();

    let stats = rx.stats();
    assert_eq!(stats.received, COUNT as u64);
    assert_eq!(stats.lost, 0);
    assert_eq!(stats.duplicate, 0);
    assert_eq!(stats.out_of_order, 0);
    assert_eq!(stats.malformed, 0);
}

#[tokio::test]
async fn udp_malformed_datagrams_are_counted_not_fatal() {
    let mut rx = UdpReceiver::bind(0).await.unwrap();
    let port = rx.local_port().unwrap();
    let tx = UdpSender::connect("127.0.0.1", port).await.unwrap();

    // Three malformed datagrams: bad magic, truncated payload, reserved type.
    let garbage_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    garbage_sock
        .send_to(&[0x00, 0x01, 0x02, 0x03, 0x04], ("127.0.0.1", port))
        .await
        .unwrap();
    garbage_sock
        .send_to(
            &[0xA7, 0x00, 0x00, 0x01, 0x00, 0x04, 0xDE, 0xAD],
            ("127.0.0.1", port),
        )
        .await
        .unwrap();
    garbage_sock
        .send_to(&[0xA7, 0x7F, 0x00, 0x00, 0x00, 0x00], ("127.0.0.1", port))
        .await
        .unwrap();

    // Then valid traffic must still flow.
    for seq in 0..5u16 {
        tx.send_packet(&gen_packet(seq, 64)).await.unwrap();
    }

    for seq in 0..5u16 {
        let pkt = rx
            .recv_packet(Duration::from_secs(5))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("timed out waiting for seq {seq}"));
        assert_packet_matches(&pkt, seq, 64);
    }

    let stats = rx.stats();
    assert_eq!(stats.malformed, 3, "malformed datagrams must be counted");
    assert_eq!(stats.received, 5);
    assert_eq!(stats.lost, 0);
}

#[tokio::test]
async fn tcp_loopback_1000_packets() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let sender = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut tx = TcpPacketStream::new(stream);
        for seq in 0..COUNT {
            tx.send_packet(&gen_packet(seq, PAYLOAD_SIZE))
                .await
                .unwrap();
        }
    });

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut rx = TcpPacketStream::new(stream);
    for seq in 0..COUNT {
        let pkt = rx
            .next_packet()
            .await
            .unwrap_or_else(|e| panic!("tcp read failed at seq {seq}: {e}"));
        assert_packet_matches(&pkt, seq, PAYLOAD_SIZE);
    }
    sender.await.unwrap();
}

#[tokio::test]
async fn tcp_resync_after_garbage_prefix() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Garbage: 17 random bytes that do not contain a valid header, plus a
    // valid packet. The receiver must resync onto the packet's magic.
    let garbage: Vec<u8> = (0..17u8).map(|i| i.wrapping_mul(37) ^ 0x5A).collect();

    let sender = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut tx = TcpPacketStream::new(stream);
        let pkt = gen_packet(9, 100);
        let wire = encode(&pkt);
        tx.get_mut().write_all(&garbage).await.unwrap();
        tx.get_mut().write_all(&wire).await.unwrap();
    });

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut rx = TcpPacketStream::new(stream);
    let pkt = rx
        .next_packet()
        .await
        .expect("resync must recover the packet");
    assert_packet_matches(&pkt, 9, 100);
    sender.await.unwrap();
}

#[tokio::test]
async fn tcp_two_packets_back_to_back() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let sender = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut tx = TcpPacketStream::new(stream);
        for seq in [0u16, 1, 2, 0xFFFE, 0xFFFF, 0x0000] {
            tx.send_packet(&gen_packet(seq, 960)).await.unwrap();
        }
    });

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut rx = TcpPacketStream::new(stream);
    for seq in [0u16, 1, 2, 0xFFFE, 0xFFFF, 0x0000] {
        let pkt = rx.next_packet().await.unwrap();
        assert_packet_matches(&pkt, seq, 960);
    }
    sender.await.unwrap();
}

// ---------------------------------------------------------------------------
// Bluetooth (feature-gated, ignored unless run explicitly)
// ---------------------------------------------------------------------------

#[cfg(feature = "bluetooth")]
mod bluetooth {
    #[tokio::test]
    #[ignore = "requires Bluetooth hardware and a BlueZ D-Bus stack"]
    async fn rfcomm_adapter_presence() {
        let session = match bluer::Session::new().await {
            Ok(s) => s,
            Err(e) => {
                println!("SKIP: no BlueZ D-Bus session: {e}");
                return;
            }
        };
        let names = match session.adapter_names().await {
            Ok(n) => n,
            Err(e) => {
                println!("SKIP: cannot enumerate adapters: {e}");
                return;
            }
        };
        if names.is_empty() {
            println!("SKIP: no Bluetooth adapters present");
            return;
        }
        println!(
            "INFO: Bluetooth adapters present ({}), not attempting real RFCOMM connections",
            names.join(", ")
        );
    }
}
