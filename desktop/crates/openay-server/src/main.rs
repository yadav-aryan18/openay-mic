//! OpenAY Mic desktop receiver: receives OpenAY audio packets (UDP/TCP),
//! decodes them, feeds a jitter buffer, and exposes a native PipeWire
//! virtual microphone source node (`openay_mic`).
//!
//! The receive/jitter pipeline works without PipeWire; the virtual source
//! is compiled in with `--features pipewire`.

mod ingest;
#[cfg(feature = "pipewire")]
mod pw;

use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "pipewire")]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use openay_jitter::{JitterBuffer, MAX_PREBUFFER_MS, MIN_PREBUFFER_MS};
use openay_protocol::PayloadType;

use crate::ingest::Ingest;

/// Samples per second (protocol-fixed: 48 kHz mono).
const SAMPLE_RATE: usize = 48_000;
/// Interval between stats lines.
const STATS_INTERVAL: Duration = Duration::from_secs(5);
/// Largest possible wire datagram: 6-byte header + 65535-byte payload.
const MAX_DATAGRAM: usize = 65541;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
enum TransportArg {
    Udp,
    Tcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
enum CodecArg {
    /// Accept either PCM or Opus payloads, per packet.
    Auto,
    /// Only raw PCM payloads are accepted.
    Pcm,
    /// Only Opus payloads are accepted.
    Opus,
}

impl CodecArg {
    fn only(self) -> Option<PayloadType> {
        match self {
            CodecArg::Auto => None,
            CodecArg::Pcm => Some(PayloadType::Pcm),
            CodecArg::Opus => Some(PayloadType::Opus),
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "openay-server",
    version,
    about = "OpenAY Mic desktop receiver: network -> jitter buffer -> PipeWire virtual microphone"
)]
struct Args {
    /// Transport to receive audio on.
    #[arg(long, value_enum, default_value = "udp")]
    transport: TransportArg,
    /// Port to listen on.
    #[arg(long, default_value_t = 41700)]
    port: u16,
    /// Address to bind.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    /// Which payload types to accept (auto = both, per packet).
    #[arg(long, value_enum, default_value = "auto")]
    codec: CodecArg,
    /// Prebuffer target in ms before streaming starts (clamped to 5..=20).
    #[arg(long, default_value_t = 10.0)]
    target_ms: f32,
    /// Jitter buffer capacity in ms of audio.
    #[arg(long, default_value_t = 100.0)]
    capacity_ms: f32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let target_ms = args.target_ms.clamp(MIN_PREBUFFER_MS, MAX_PREBUFFER_MS);
    if (args.target_ms - target_ms).abs() > f32::EPSILON {
        eprintln!(
            "openay-server: --target-ms {} clamped to [{MIN_PREBUFFER_MS}, {MAX_PREBUFFER_MS}] => {target_ms}",
            args.target_ms
        );
    }

    // ms -> samples: capacity_ms * 48 samples per ms (48 kHz).
    let capacity_samples = (args.capacity_ms * SAMPLE_RATE as f32 / 1000.0) as usize;
    let jitter = Arc::new(JitterBuffer::new(capacity_samples));
    let quit = Arc::new(AtomicBool::new(false));

    #[cfg(feature = "pipewire")]
    let (pw_thread, pw_setup_rx) = {
        let streaming = Arc::new(AtomicBool::new(false));
        let (setup_tx, setup_rx) = mpsc::channel();
        let shared = pw::PwShared {
            jitter: jitter.clone(),
            streaming: streaming.clone(),
            quit: quit.clone(),
            target_samples: (target_ms * SAMPLE_RATE as f32 / 1000.0).ceil() as usize,
        };
        let thread = std::thread::Builder::new()
            .name("openay-pipewire".into())
            .spawn(move || pw::run_pipewire(shared, setup_tx))
            .context("spawning PipeWire thread")?;
        (thread, setup_rx)
    };

    #[cfg(not(feature = "pipewire"))]
    eprintln!("openay-server: built without PipeWire support — network+jitter only");

    let ingest = Arc::new(Mutex::new(Ingest::new(jitter.clone(), args.codec.only())));
    let net = tokio::spawn(run_network(
        args.clone(),
        ingest.clone(),
        jitter.clone(),
        quit.clone(),
    ));

    #[cfg(feature = "pipewire")]
    let mut pw_error: Option<String> = None;
    #[cfg(feature = "pipewire")]
    let mut pw_setup = Some(tokio::task::spawn_blocking(move || pw_setup_rx.recv()));

    // Wait for Ctrl-C, or for an early PipeWire setup failure. Whichever
    // fires first ends the wait (both arms are terminal).
    #[cfg(feature = "pipewire")]
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            quit.store(true, Ordering::Relaxed);
        }
        res = pw_setup.as_mut().expect("pw_setup pending"), if pw_setup.is_some() => {
            quit.store(true, Ordering::Relaxed);
            // Only an Err message is ever sent (setup failed). A closed
            // channel means the thread exited without the main loop
            // having been stopped — report that too.
            let msg = match res {
                Ok(Ok(Err(e))) => format!("PipeWire setup failed: {e}"),
                Ok(Ok(Ok(()))) => "PipeWire thread exited unexpectedly".to_string(),
                Ok(Err(_)) | Err(_) => {
                    "PipeWire setup channel closed unexpectedly".to_string()
                }
            };
            pw_error = Some(msg);
        }
    }

    #[cfg(not(feature = "pipewire"))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("waiting for Ctrl-C")?;
        quit.store(true, Ordering::Relaxed);
    }

    // The network task notices `quit` within ~200 ms and returns the final
    // stats line.
    let final_stats = net.await.context("network task panicked")??;

    #[cfg(feature = "pipewire")]
    {
        // Give the loop up to ~3 s to wind down (it polls the quit flag
        // every 50 ms), then reap the thread.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if pw_thread.is_finished() {
                break;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "openay-server: PipeWire thread did not exit within 3 s; leaving it to process exit"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        match pw_thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("openay-server: PipeWire error: {e:#}"),
            Err(_) => eprintln!("openay-server: PipeWire thread panicked"),
        }
    }

    println!("{final_stats}");

    #[cfg(feature = "pipewire")]
    if let Some(msg) = pw_error {
        anyhow::bail!("{msg}");
    }
    Ok(())
}

/// Run the receive pipeline until `quit` is set, then return the final
/// stats line.
async fn run_network(
    args: Args,
    ingest: Arc<Mutex<Ingest>>,
    jitter: Arc<JitterBuffer>,
    quit: Arc<AtomicBool>,
) -> Result<String> {
    match args.transport {
        TransportArg::Udp => udp_loop(&args, ingest, jitter, quit).await,
        TransportArg::Tcp => tcp_loop(&args, ingest, jitter, quit).await,
    }
}

/// UDP receive loop: one datagram == one packet (protocol spec). Malformed
/// datagrams are dropped and counted, never fatal.
async fn udp_loop(
    args: &Args,
    ingest: Arc<Mutex<Ingest>>,
    jitter: Arc<JitterBuffer>,
    quit: Arc<AtomicBool>,
) -> Result<String> {
    let socket = tokio::net::UdpSocket::bind(format!("{}:{}", args.bind, args.port))
        .await
        .with_context(|| format!("binding UDP {}:{}", args.bind, args.port))?;
    eprintln!(
        "openay-server: UDP listening on {}:{}",
        args.bind, args.port
    );

    let mut buf = [0u8; MAX_DATAGRAM];
    let mut last_stats = Instant::now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            r = socket.recv_from(&mut buf) => {
                let (n, _src) = r.context("UDP recv error")?;
                match openay_protocol::decode(&buf[..n]) {
                    Ok(pkt) => {
                        let mut g = ingest.lock().expect("ingest mutex poisoned");
                        if g.ingest_packet(pkt.kind, pkt.seq, &pkt.payload).is_err() {
                            // Malformed/undecodable payloads are counted
                            // inside ingest_packet; nothing to do here.
                        }
                    }
                    Err(_) => {
                        ingest.lock().expect("ingest mutex poisoned").malformed += 1;
                    }
                }
            }
        }
        if quit.load(Ordering::Relaxed) {
            break;
        }
        if last_stats.elapsed() >= STATS_INTERVAL {
            println!("{}", stats_line("udp", &ingest, &jitter));
            last_stats = Instant::now();
        }
    }
    Ok(stats_line("udp", &ingest, &jitter))
}

/// TCP receive loop: back-to-back framed packets via `TcpPacketStream`; each
/// accepted connection is handled by its own task, so one stalled client
/// cannot block ingestion from others.
async fn tcp_loop(
    args: &Args,
    ingest: Arc<Mutex<Ingest>>,
    jitter: Arc<JitterBuffer>,
    quit: Arc<AtomicBool>,
) -> Result<String> {
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", args.bind, args.port))
        .await
        .with_context(|| format!("binding TCP {}:{}", args.bind, args.port))?;
    eprintln!(
        "openay-server: TCP listening on {}:{}",
        args.bind, args.port
    );

    let mut last_stats = Instant::now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            r = listener.accept() => {
                let (stream, peer) = r.context("accept error")?;
                eprintln!("openay-server: TCP connection from {peer}");
                let conn_ingest = ingest.clone();
                let conn_quit = quit.clone();
                tokio::spawn(async move {
                    let mut framed = openay_transport::tcp::TcpPacketStream::new(stream);
                    loop {
                        if conn_quit.load(Ordering::Relaxed) {
                            break;
                        }
                        match framed.next_packet().await {
                            Ok(pkt) => {
                                let mut g = conn_ingest.lock().expect("ingest mutex poisoned");
                                if g.ingest_packet(pkt.kind, pkt.seq, &pkt.payload).is_err() {
                                    // Counted inside ingest_packet.
                                }
                            }
                            Err(e) => {
                                eprintln!("openay-server: TCP connection {peer} closed: {e}");
                                break;
                            }
                        }
                    }
                });
            }
        }
        if quit.load(Ordering::Relaxed) {
            break;
        }
        if last_stats.elapsed() >= STATS_INTERVAL {
            println!("{}", stats_line("tcp", &ingest, &jitter));
            last_stats = Instant::now();
        }
    }
    Ok(stats_line("tcp", &ingest, &jitter))
}

/// The canonical server stats line, printed every 5 s and once at shutdown:
/// `SRV transport=<t> received=<n> lost=<n> dup=<d> ooo=<o> malformed=<m>
/// overruns=<r> underruns=<u> fill_ms=<F.1>`
fn stats_line(transport: &str, ingest: &Mutex<Ingest>, jitter: &JitterBuffer) -> String {
    let g = ingest.lock().expect("ingest mutex poisoned");
    let fill_ms = jitter.available() as f32 / SAMPLE_RATE as f32 * 1000.0;
    format!(
        "SRV transport={transport} received={} lost={} dup={} ooo={} malformed={} \
         overruns={} underruns={} fill_ms={fill_ms:.1}",
        g.received,
        g.lost,
        g.duplicate,
        g.out_of_order,
        g.malformed,
        jitter.overruns(),
        jitter.underruns(),
    )
}
