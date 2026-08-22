//! End-to-end tests: drive [`openay_proxy::run_proxy`] in-process over real
//! loopback UDP sockets and stop it through the `quit` flag.
//!
//! The proxy's listen socket binds asynchronously, so each test first sends
//! probe datagrams until one is observed at the receiver. Probes consume
//! one decision each, so the expected stats are recomputed from the public
//! [`DecisionEngine`] over the exact decision window (probes, then batch).

use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use openay_proxy::{DecisionEngine, Profile, ProxyConfig, ProxyStats, run_proxy};
use tokio::net::UdpSocket;
use tokio::time::Instant;

/// Pick an OS-assigned free port. The ephemeral socket is closed before the
/// proxy binds it; on loopback the race window is negligible.
fn free_port() -> u16 {
    let sock = StdUdpSocket::bind(("127.0.0.1", 0)).expect("bind ephemeral socket");
    sock.local_addr().expect("local addr").port()
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Receive datagrams until `max` have arrived or the stream goes quiet for
/// `quiet` (or the overall `total` budget expires). Returns how many
/// datagrams arrived.
async fn receive_until_quiet(
    sock: &UdpSocket,
    max: usize,
    quiet: Duration,
    total: Duration,
) -> usize {
    let mut got = 0usize;
    let mut buf = vec![0u8; 2048];
    let deadline = Instant::now() + total;
    while got < max {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = (deadline - now).min(quiet);
        match tokio::time::timeout(wait, sock.recv_from(&mut buf)).await {
            Ok(Ok(_)) => got += 1,
            // Error or quiet period elapsed: no more datagrams expected.
            Ok(Err(_)) | Err(_) => break,
        }
    }
    got
}

async fn send_one(target: SocketAddr) -> Result<()> {
    let sender = UdpSocket::bind(("127.0.0.1", 0)).await?;
    sender.send_to(&0u64.to_be_bytes(), target).await?;
    Ok(())
}

/// Send `count` datagrams (payload = 8-byte sequence number) to `target`,
/// paced at 1 ms apart. Real audio arrives at ~100 pps (10 ms apart); a
/// zero-paced blast of tiny datagrams is an unrealistic worst case that the
/// loopback receive-buffer accounting is not sized for.
async fn send_datagrams(count: usize, target: SocketAddr) -> Result<()> {
    let sender = UdpSocket::bind(("127.0.0.1", 0)).await?;
    for i in 0..count as u64 {
        sender.send_to(&i.to_be_bytes(), target).await?;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

/// Spawn the proxy, then wait until its listen socket is actually bound by
/// sending probe datagrams and waiting for them at the single receiver that
/// is also used for the batch (so nothing is lost between readiness and
/// collection). Returns the number of probes used (1 on the usual first try).
async fn spawn_and_ready(
    profile: Profile,
    seed: u64,
) -> Result<(
    u16,
    u16,
    UdpSocket,
    usize,
    Arc<AtomicBool>,
    tokio::task::JoinHandle<Result<ProxyStats>>,
)> {
    let listen_port = free_port();
    let forward_port = free_port();
    let quit = Arc::new(AtomicBool::new(false));
    let q = quit.clone();
    let config = ProxyConfig::new(addr(listen_port), addr(forward_port), profile).with_seed(seed);
    let handle = tokio::spawn(async move { run_proxy(config, q).await });

    let receiver = UdpSocket::bind(addr(forward_port)).await?;
    let mut probes = 0;
    loop {
        probes += 1;
        send_one(addr(listen_port)).await?;
        if receive_until_quiet(&receiver, 1, Duration::from_millis(250), Duration::from_secs(2)).await == 1 {
            break;
        }
        assert!(
            probes < 50,
            "proxy did not start listening within the probe budget"
        );
    }
    Ok((listen_port, forward_port, receiver, probes, quit, handle))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_forwards_every_datagram() -> Result<()> {
    let (listen_port, _forward_port, receiver, probes, quit, handle) =
        spawn_and_ready(Profile::Clean, 1).await?;

    let count = 100;
    // Collect concurrently with the send: the receiver socket's kernel
    // buffer cannot hold a full batch (per-skb rcvbuf accounting), so a
    // collect-after-send pattern would lose datagrams to overflow.
    let send_task = tokio::spawn(send_datagrams(count, addr(listen_port)));
    let got = receive_until_quiet(&receiver, count * 2, Duration::from_secs(1), Duration::from_secs(5)).await;
    send_task.await.expect("sender task")?;

    quit.store(true, Ordering::Relaxed);
    let stats = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("proxy shut down")??;
    assert_eq!(got, count, "clean profile must forward the whole batch");
    assert_eq!(stats.forwarded, (count + probes) as u64);
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.duplicated, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss2_drops_exactly_the_simulated_count() -> Result<()> {
    let (listen_port, _forward_port, receiver, probes, quit, handle) =
        spawn_and_ready(Profile::Loss2, 99).await?;

    let count = 500;
    // Collect concurrently with the send (see `clean_forwards_every_datagram`).
    let send_task = tokio::spawn(send_datagrams(count, addr(listen_port)));
    let got = receive_until_quiet(&receiver, count, Duration::from_secs(1), Duration::from_secs(5)).await;
    send_task.await.expect("sender task")?;

    quit.store(true, Ordering::Relaxed);
    let stats = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("proxy shut down")??;

    // Recompute the deterministic decision counts over the exact window.
    let mut engine = DecisionEngine::new(Profile::Loss2, 99);
    let probe_drops = (0..probes).filter(|_| engine.decide().is_drop()).count() as u64;
    let batch_drops = (0..count).filter(|_| engine.decide().is_drop()).count() as u64;

    assert_eq!(stats.dropped, probe_drops + batch_drops);
    assert_eq!(stats.forwarded, (count + probes) as u64 - stats.dropped);
    assert_eq!(got, count - batch_drops as usize);
    assert!(batch_drops > 0, "loss2 with seed 99 must drop batch datagrams");
    assert!(
        (0.01..=0.05).contains(&(stats.dropped as f64 / (count + probes) as f64)),
        "loss2 drop rate {} outside band",
        stats.dropped as f64 / (count + probes) as f64
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jitter30_loses_nothing_duplicates_sometimes() -> Result<()> {
    // Seed 42: verified that the decision window includes duplicates, so
    // the assertions below hold deterministically.
    let (listen_port, _forward_port, receiver, probes, quit, handle) =
        spawn_and_ready(Profile::Jitter30, 42).await?;

    let count = 300;
    // Collect concurrently with the send (see `clean_forwards_every_datagram`).
    // Delays are at most 60 ms; a quiet period of 1 s is ample.
    let send_task = tokio::spawn(send_datagrams(count, addr(listen_port)));
    let got = receive_until_quiet(&receiver, count + 50, Duration::from_secs(1), Duration::from_secs(5)).await;
    send_task.await.expect("sender task")?;

    quit.store(true, Ordering::Relaxed);
    let stats = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("proxy shut down")??;

    // Recompute the deterministic duplicate counts over the exact window.
    let mut engine = DecisionEngine::new(Profile::Jitter30, 42);
    let probe_dups = (0..probes).filter(|_| engine.decide().duplicates()).count() as u64;
    let batch_dups = (0..count).filter(|_| engine.decide().duplicates()).count() as u64;

    assert_eq!(stats.dropped, 0, "jitter30 never drops");
    assert_eq!(stats.duplicated, probe_dups + batch_dups);
    // Every original (probes + batch) is forwarded exactly once, plus dups.
    assert_eq!(stats.forwarded, (count + probes) as u64 + stats.duplicated);
    assert_eq!(got, count + batch_dups as usize, "batch records at receiver");
    assert!(batch_dups > 0, "seed 42's batch window must contain a duplicate");
    Ok(())
}
