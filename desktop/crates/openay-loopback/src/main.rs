//! OpenAY Mic loopback sender/receiver/bench CLI.
//!
//! Exact CLI (defaults shown):
//!
//! ```text
//! openay-loopback send-udp <host> <port> <count> [payload_size=480] [interval_us=0]
//! openay-loopback recv-udp <port> <count> [payload_size=480]
//! openay-loopback send-tcp <host> <port> <count> [payload_size=480]
//! openay-loopback recv-tcp <port> <count> [payload_size=480]
//! openay-loopback bench <udp|tcp> <port> <count> [payload_size=480]
//! ```
//!
//! All verification follows `shared/protocol.md`: alternating Pcm/Opus types
//! starting with Pcm, sequence contiguity from 0, exact payload size, and
//! xorshift(seed=seq) filler content.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use openay_protocol::{Packet, PayloadType, HEADER_LEN};
use openay_transport::{
    fill_xorshift, PacketStats, SeqEvent, SeqTracker, TcpPacketStream, UdpReceiver, UdpSender,
};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

/// Receive-side inactivity timeout ("no traffic").
const NO_TRAFFIC_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a bench run may take before we give up waiting for the receiver.
const BENCH_TIMEOUT: Duration = Duration::from_secs(60);

const USAGE: &str = "\
openay-loopback — OpenAY Mic loopback sender/receiver/bench

USAGE:
  openay-loopback send-udp <host> <port> <count> [payload_size=480] [interval_us=0]
  openay-loopback recv-udp <port> <count> [payload_size=480]
  openay-loopback send-tcp <host> <port> <count> [payload_size=480]
  openay-loopback recv-tcp <port> <count> [payload_size=480]
  openay-loopback bench <udp|tcp> <port> <count> [payload_size=480]

EXIT CODES:
  0  success (send, or recv with zero errors, or bench with p99 < 5000 us)
  1  recv verification failures / bench p99 >= 5000 us
  2  usage error
";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

async fn run(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        Some("send-udp") => {
            let (host, port, count, payload_size, interval_us) = parse_send_udp(&args[1..])?;
            send_udp(&host, port, count, payload_size, interval_us).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some("recv-udp") => {
            let (port, count, payload_size) = parse_recv(&args[1..])?;
            let ok = recv_udp(port, count, payload_size).await?;
            Ok(exit_for_ok(ok))
        }
        Some("send-tcp") => {
            let (host, port, count, payload_size) = parse_send_tcp(&args[1..])?;
            send_tcp(&host, port, count, payload_size).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some("recv-tcp") => {
            let (port, count, payload_size) = parse_recv(&args[1..])?;
            let ok = recv_tcp(port, count, payload_size).await?;
            Ok(exit_for_ok(ok))
        }
        Some("bench") => {
            let (transport, port, count, payload_size) = parse_bench(&args[1..])?;
            let ok = bench(&transport, port, count, payload_size).await?;
            Ok(exit_for_ok(ok))
        }
        _ => Err(anyhow!("unknown or missing command")),
    }
}

fn exit_for_ok(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------
// Argument parsing (literal match to the spec)
// ---------------------------------------------------------------------------

fn parse_send_udp(args: &[String]) -> Result<(String, u16, usize, usize, u64)> {
    if !(3..=5).contains(&args.len()) {
        bail!("send-udp expects: <host> <port> <count> [payload_size] [interval_us]");
    }
    let host = args[0].clone();
    let port = parse_u16(&args[1], "port")?;
    let count = parse_usize(&args[2], "count")?;
    let payload_size = parse_opt_usize(args.get(3), "payload_size", 480)?;
    let interval_us = parse_opt_u64(args.get(4), "interval_us", 0)?;
    if count == 0 {
        bail!("count must be >= 1");
    }
    Ok((host, port, count, payload_size, interval_us))
}

fn parse_send_tcp(args: &[String]) -> Result<(String, u16, usize, usize)> {
    if !(3..=4).contains(&args.len()) {
        bail!("send-tcp expects: <host> <port> <count> [payload_size]");
    }
    let host = args[0].clone();
    let port = parse_u16(&args[1], "port")?;
    let count = parse_usize(&args[2], "count")?;
    let payload_size = parse_opt_usize(args.get(3), "payload_size", 480)?;
    if count == 0 {
        bail!("count must be >= 1");
    }
    Ok((host, port, count, payload_size))
}

fn parse_recv(args: &[String]) -> Result<(u16, usize, usize)> {
    if !(2..=3).contains(&args.len()) {
        bail!("recv-* expects: <port> <count> [payload_size]");
    }
    let port = parse_u16(&args[0], "port")?;
    let count = parse_usize(&args[1], "count")?;
    let payload_size = parse_opt_usize(args.get(2), "payload_size", 480)?;
    if count == 0 {
        bail!("count must be >= 1");
    }
    Ok((port, count, payload_size))
}

fn parse_bench(args: &[String]) -> Result<(String, u16, usize, usize)> {
    if !(3..=4).contains(&args.len()) {
        bail!("bench expects: <udp|tcp> <port> <count> [payload_size]");
    }
    let transport = args[0].clone();
    if transport != "udp" && transport != "tcp" {
        bail!("bench transport must be \"udp\" or \"tcp\"");
    }
    let port = parse_u16(&args[1], "port")?;
    let count = parse_usize(&args[2], "count")?;
    let payload_size = parse_opt_usize(args.get(3), "payload_size", 480)?;
    if count == 0 {
        bail!("count must be >= 1");
    }
    if payload_size < 8 {
        bail!("bench payload_size must be >= 8 (8-byte timestamp prefix)");
    }
    Ok((transport, port, count, payload_size))
}

fn parse_u16(s: &str, what: &str) -> Result<u16> {
    s.parse::<u16>()
        .with_context(|| format!("invalid {what} '{s}' (0..=65535)"))
}

fn parse_usize(s: &str, what: &str) -> Result<usize> {
    s.parse::<usize>()
        .with_context(|| format!("invalid {what} '{s}'"))
}

fn parse_opt_usize(v: Option<&String>, what: &str, default: usize) -> Result<usize> {
    match v {
        Some(s) => s
            .parse::<usize>()
            .with_context(|| format!("invalid {what} '{s}'")),
        None => Ok(default),
    }
}

fn parse_opt_u64(v: Option<&String>, what: &str, default: u64) -> Result<u64> {
    match v {
        Some(s) => s
            .parse::<u64>()
            .with_context(|| format!("invalid {what} '{s}'")),
        None => Ok(default),
    }
}

// ---------------------------------------------------------------------------
// Packet generation
// ---------------------------------------------------------------------------

/// Generate the `seq`-th test packet: alternating Pcm/Opus starting Pcm,
/// xorshift(seed=seq) payload.
fn gen_packet(seq: u16, payload_size: usize) -> Packet {
    let kind = if seq % 2 == 0 {
        PayloadType::Pcm
    } else {
        PayloadType::Opus
    };
    let mut payload = vec![0u8; payload_size];
    fill_xorshift(&mut payload, seq as u32);
    Packet { kind, seq, payload }
}

/// Generate a bench packet: 8-byte LE monotonic-ns stamp followed by
/// xorshift(seed=seq) filler.
fn gen_bench_packet(seq: u16, stamp_ns: u64, payload_size: usize) -> Packet {
    let kind = if seq % 2 == 0 {
        PayloadType::Pcm
    } else {
        PayloadType::Opus
    };
    let mut payload = vec![0u8; payload_size];
    payload[..8].copy_from_slice(&stamp_ns.to_le_bytes());
    fill_xorshift(&mut payload[8..], seq as u32);
    Packet { kind, seq, payload }
}

// ---------------------------------------------------------------------------
// Senders
// ---------------------------------------------------------------------------

async fn send_udp(
    host: &str,
    port: u16,
    count: usize,
    payload_size: usize,
    interval_us: u64,
) -> Result<()> {
    let tx = UdpSender::connect(host, port)
        .await
        .with_context(|| format!("cannot connect UDP {host}:{port}"))?;
    for seq in 0..count as u64 {
        let pkt = gen_packet(seq as u16, payload_size);
        tx.send_packet(&pkt)
            .await
            .with_context(|| format!("UDP send failed at seq {seq}"))?;
        if interval_us > 0 {
            tokio::time::sleep(Duration::from_micros(interval_us)).await;
        }
    }
    let total_wire_bytes = count * (HEADER_LEN + payload_size);
    println!("SENT count={count} bytes={total_wire_bytes}");
    Ok(())
}

async fn send_tcp(host: &str, port: u16, count: usize, payload_size: usize) -> Result<()> {
    let stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("cannot connect TCP {host}:{port}"))?;
    // Mic traffic is latency-critical: disable Nagle + delayed-ACK batching.
    stream.set_nodelay(true).context("cannot set TCP_NODELAY")?;
    let mut tx = TcpPacketStream::new(stream);
    for seq in 0..count as u64 {
        let pkt = gen_packet(seq as u16, payload_size);
        tx.send_packet(&pkt)
            .await
            .with_context(|| format!("TCP send failed at seq {seq}"))?;
    }
    // Clean close so a receiver sees a well-formed end of stream.
    let _ = tx.get_mut().shutdown().await;
    let total_wire_bytes = count * (HEADER_LEN + payload_size);
    println!("SENT count={count} bytes={total_wire_bytes}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Receivers (verification)
// ---------------------------------------------------------------------------

/// Verifies type alternation, seq contiguity from 0, payload length and
/// xorshift content. Tracks the canonical stats line.
struct RecvVerifier {
    tracker: SeqTracker,
    stats: PacketStats,
    payload_size: usize,
}

impl RecvVerifier {
    fn new(payload_size: usize) -> Self {
        RecvVerifier {
            tracker: SeqTracker::new(),
            stats: PacketStats::default(),
            payload_size,
        }
    }

    fn check(&mut self, pkt: &Packet) {
        self.stats.received += 1;
        match self.tracker.update(pkt.seq) {
            SeqEvent::Gap(n) => self.stats.lost += n as u64,
            SeqEvent::Duplicate => self.stats.duplicate += 1,
            SeqEvent::Reorder => self.stats.out_of_order += 1,
            _ => {}
        }
        let expected_kind = if pkt.seq % 2 == 0 {
            PayloadType::Pcm
        } else {
            PayloadType::Opus
        };
        if pkt.kind != expected_kind {
            self.stats.content_errors += 1;
        }
        if pkt.payload.len() != self.payload_size {
            self.stats.content_errors += 1;
        } else {
            let mut expect = vec![0u8; self.payload_size];
            fill_xorshift(&mut expect, pkt.seq as u32);
            if pkt.payload != expect {
                self.stats.content_errors += 1;
            }
        }
    }

    fn finished_ok(&self, count: usize) -> bool {
        self.stats.received == count as u64
            && self.stats.lost == 0
            && self.stats.duplicate == 0
            && self.stats.out_of_order == 0
            && self.stats.malformed == 0
            && self.stats.content_errors == 0
    }
}

async fn recv_udp(port: u16, count: usize, payload_size: usize) -> Result<bool> {
    let mut rx = UdpReceiver::bind(port)
        .await
        .with_context(|| format!("cannot bind UDP 127.0.0.1:{port}"))?;
    // Larger kernel buffer so a flat-out sender (interval_us=0) does not
    // overflow the socket queue; the kernel may cap it at rmem_max.
    let _ = rx.set_recv_buffer_size(4 * 1024 * 1024);

    let mut verifier = RecvVerifier::new(payload_size);
    while verifier.stats.received < count as u64 {
        match rx.recv_packet(NO_TRAFFIC_TIMEOUT).await {
            Ok(Some(pkt)) => verifier.check(&pkt),
            Ok(None) => {
                eprintln!(
                    "recv-udp: timeout after {} of {count} packets (15 s without traffic)",
                    verifier.stats.received
                );
                break;
            }
            Err(e) => {
                eprintln!("recv-udp: receive error: {e}");
                break;
            }
        }
    }
    // Malformed datagrams are counted by the receiver itself.
    verifier.stats.malformed = rx.stats().malformed;
    println!("{}", verifier.stats.render());
    Ok(verifier.finished_ok(count))
}

async fn recv_tcp(port: u16, count: usize, payload_size: usize) -> Result<bool> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("cannot bind TCP 127.0.0.1:{port}"))?;
    let (stream, peer) = listener
        .accept()
        .await
        .with_context(|| "recv-tcp: waiting for the sender connection")?;
    stream.set_nodelay(true).context("cannot set TCP_NODELAY")?;
    eprintln!("recv-tcp: accepted connection from {peer}");
    let mut rx = TcpPacketStream::new(stream);

    let mut verifier = RecvVerifier::new(payload_size);
    while verifier.stats.received < count as u64 {
        match tokio::time::timeout(NO_TRAFFIC_TIMEOUT, rx.next_packet()).await {
            Ok(Ok(pkt)) => verifier.check(&pkt),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!(
                    "recv-tcp: sender closed after {} of {count} packets",
                    verifier.stats.received
                );
                break;
            }
            Ok(Err(e)) => {
                eprintln!("recv-tcp: stream error: {e}");
                break;
            }
            Err(_elapsed) => {
                eprintln!(
                    "recv-tcp: timeout after {} of {count} packets (15 s without traffic)",
                    verifier.stats.received
                );
                break;
            }
        }
    }
    println!("{}", verifier.stats.render());
    Ok(verifier.finished_ok(count))
}

// ---------------------------------------------------------------------------
// Bench
// ---------------------------------------------------------------------------

/// Result sent by the bench receiver task.
struct BenchResult {
    deltas_ns: Vec<u64>,
    content_errors: u64,
}

/// Receiver half of the bench: collect one-way delays (receiver's monotonic
/// now minus the stamp embedded by the sender).
async fn bench_receive_udp(
    mut rx: UdpReceiver,
    count: usize,
    payload_size: usize,
    t0: std::time::Instant,
    done: tokio::sync::mpsc::Sender<BenchResult>,
) {
    let mut deltas = Vec::with_capacity(count);
    let mut content_errors = 0u64;
    for _ in 0..count {
        match rx.recv_packet(BENCH_TIMEOUT).await {
            Ok(Some(pkt)) => {
                let (delta, bad) = bench_check_packet(&pkt, payload_size, t0);
                if bad {
                    content_errors += 1;
                }
                deltas.push(delta);
            }
            Ok(None) => {
                eprintln!("bench-udp: receiver timeout waiting for a packet");
                break;
            }
            Err(e) => {
                eprintln!("bench-udp: receive error: {e}");
                break;
            }
        }
    }
    let _ = done
        .send(BenchResult {
            deltas_ns: deltas,
            content_errors,
        })
        .await;
}

/// Receiver half of the bench over TCP.
async fn bench_receive_tcp(
    listener: TcpListener,
    count: usize,
    payload_size: usize,
    t0: std::time::Instant,
    done: tokio::sync::mpsc::Sender<BenchResult>,
) {
    let (stream, _peer) = match listener.accept().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bench-tcp: accept error: {e}");
            let _ = done
                .send(BenchResult {
                    deltas_ns: Vec::new(),
                    content_errors: 0,
                })
                .await;
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let mut rx = TcpPacketStream::new(stream);
    let mut deltas = Vec::with_capacity(count);
    let mut content_errors = 0u64;
    for _ in 0..count {
        match tokio::time::timeout(BENCH_TIMEOUT, rx.next_packet()).await {
            Ok(Ok(pkt)) => {
                let (delta, bad) = bench_check_packet(&pkt, payload_size, t0);
                if bad {
                    content_errors += 1;
                }
                deltas.push(delta);
            }
            Ok(Err(e)) => {
                eprintln!("bench-tcp: stream error: {e}");
                break;
            }
            Err(_) => {
                eprintln!("bench-tcp: receiver timeout waiting for a packet");
                break;
            }
        }
    }
    let _ = done
        .send(BenchResult {
            deltas_ns: deltas,
            content_errors,
        })
        .await;
}

/// Verify a bench packet's layout and return (one-way delay ns, bad content?).
fn bench_check_packet(pkt: &Packet, payload_size: usize, t0: std::time::Instant) -> (u64, bool) {
    let mut bad = false;
    if pkt.payload.len() != payload_size {
        return (0, true);
    }
    let stamp = u64::from_le_bytes(pkt.payload[..8].try_into().unwrap());
    let now = t0.elapsed().as_nanos() as u64;
    let delta = now.saturating_sub(stamp);

    let mut expect = vec![0u8; payload_size - 8];
    fill_xorshift(&mut expect, pkt.seq as u32);
    if pkt.payload[8..] != expect[..] {
        bad = true;
    }
    (delta, bad)
}

async fn bench(transport: &str, port: u16, count: usize, payload_size: usize) -> Result<bool> {
    let t0 = std::time::Instant::now();
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel(1);

    match transport {
        "udp" => {
            // Bind before spawning so the socket is ready when we start sending.
            let rx = UdpReceiver::bind(port)
                .await
                .with_context(|| format!("cannot bind UDP 127.0.0.1:{port}"))?;
            let _ = rx.set_recv_buffer_size(4 * 1024 * 1024);
            let task = tokio::spawn(bench_receive_udp(rx, count, payload_size, t0, done_tx));

            let tx = UdpSender::connect("127.0.0.1", port)
                .await
                .with_context(|| format!("cannot connect UDP 127.0.0.1:{port}"))?;
            for seq in 0..count as u64 {
                let stamp = t0.elapsed().as_nanos() as u64;
                let pkt = gen_bench_packet(seq as u16, stamp, payload_size);
                tx.send_packet(&pkt)
                    .await
                    .with_context(|| format!("bench-udp: send failed at seq {seq}"))?;
            }
            task.await.context("bench-udp: receiver task panicked")?;
        }
        "tcp" => {
            let listener = TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot bind TCP 127.0.0.1:{port}"))?;
            let task = tokio::spawn(bench_receive_tcp(
                listener,
                count,
                payload_size,
                t0,
                done_tx,
            ));

            let stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot connect TCP 127.0.0.1:{port}"))?;
            stream.set_nodelay(true).context("cannot set TCP_NODELAY")?;
            let mut tx = TcpPacketStream::new(stream);
            for seq in 0..count as u64 {
                let stamp = t0.elapsed().as_nanos() as u64;
                let pkt = gen_bench_packet(seq as u16, stamp, payload_size);
                tx.send_packet(&pkt)
                    .await
                    .with_context(|| format!("bench-tcp: send failed at seq {seq}"))?;
            }
            let _ = tx.get_mut().shutdown().await;
            task.await.context("bench-tcp: receiver task panicked")?;
        }
        _ => unreachable!("transport validated at parse time"),
    }

    let result = tokio::time::timeout(BENCH_TIMEOUT, done_rx.recv())
        .await
        .context("bench: timed out waiting for the receiver")?
        .ok_or_else(|| anyhow!("bench: receiver channel closed without a result"))?;

    if result.content_errors > 0 {
        eprintln!(
            "bench: warning: {} packets had content errors (timestamps or filler)",
            result.content_errors
        );
    }

    let n = result.deltas_ns.len();
    if n == 0 {
        bail!("bench: no packets received");
    }
    let mut sorted = result.deltas_ns.clone();
    sorted.sort_unstable();

    let pct = |p: f64| -> u64 {
        let idx = ((p / 100.0) * n as f64).ceil().max(1.0) as usize - 1;
        sorted[idx] / 1000 // ns -> us (integer division)
    };
    let p50 = pct(50.0);
    let p95 = pct(95.0);
    let p99 = pct(99.0);
    let max_us = *sorted.last().unwrap() / 1000;

    println!(
        "BENCH transport={transport} count={n} p50_us={p50} p95_us={p95} p99_us={p99} max_us={max_us}"
    );
    Ok(p99 < 5000)
}
