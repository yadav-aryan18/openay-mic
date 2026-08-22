//! End-to-end tests: drive [`openay_proxy::run_proxy`] in-process over real
//! loopback UDP sockets and stop it through the `quit` flag.
//!
//! The proxy's listen socket binds asynchronously, so each test first sends
//! probe datagrams until one is observed at the receiver. A probe sent
//! before `bind()` completes would be silently dropped by the kernel while
//! still counting in the sender's bookkeeping, so the number of decision
//! slots the probes consumed is NOT assumed up front. Instead, after
//! shutdown the exact consumed window is derived from the proxy's own
//! totals — every decision ends as exactly one of:
//!
//! - forward immediate / delayed / duplicate-original pair: contributes one
//!   to `delayed`, plus one to `forwarded` when it also produced the
//!   immediate duplicate copy;
//! - drop: contributes one to `dropped`.
//!
//! so `decisions = forwarded − duplicated + dropped`, and the expected
//! batch statistics are recomputed from the LAST `count` draws of that
//! window (the leading draws are whatever the probes consumed).

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
/// collection). How many decision slots the probes consumed is unknown by
/// construction (see the module docs); callers derive it from the final
/// [`ProxyStats`] via [`decisions_consumed`].
async fn spawn_and_ready(
    profile: Profile,
    seed: u64,
) -> Result<(
    u16,
    u16,
    UdpSocket,
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
    let mut probes = 0u32;
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
    Ok((listen_port, forward_port, receiver, quit, handle))
}

/// Decision slots consumed by the proxy (see module docs).
fn decisions_consumed(stats: &ProxyStats) -> u64 {
    stats.forwarded - stats.duplicated + stats.dropped
}

/// Recompute the deterministic decisions over the whole consumed window and
/// return the aggregate counters plus the LAST `batch` actions (the real
/// batch; the leading draws belong to the readiness probes).
fn replay(profile: Profile, seed: u64, decisions: u64, batch: usize) -> (ProxyStats, Vec<openay_proxy::Action>) {
    let mut engine = DecisionEngine::new(profile, seed);
    let mut totals = ProxyStats::default();
    let mut tail = Vec::with_capacity(batch);
    for i in 0..decisions {
        let action = engine.decide();
        match action {
            openay_proxy::Action::ForwardImmediate => totals.forwarded += 1,
            openay_proxy::Action::ForwardDelayed(_) => {}
            openay_proxy::Action::ForwardImmediatePlusDelayed(_) => {
                totals.forwarded += 1;
                totals.duplicated += 1;
            }
            openay_proxy::Action::Drop => totals.dropped += 1,
        }
        if i >= decisions - batch as u64 {
            tail.push(action);
        }
    }
    (totals, tail)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_forwards_every_datagram() -> Result<()> {
    let (listen_port, _forward_port, receiver, quit, handle) =
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
    let decisions = decisions_consumed(&stats);
    assert!(decisions >= count as u64, "window must cover the batch");
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.duplicated, 0);
    // Clean forwards every datagram it decided on.
    assert_eq!(stats.forwarded, decisions);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss2_drops_exactly_the_simulated_count() -> Result<()> {
    let (listen_port, _forward_port, receiver, quit, handle) =
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

    // Replay the exact consumed window (probes included, however many).
    let decisions = decisions_consumed(&stats);
    assert!(decisions >= count as u64, "window must cover the batch");
    let (_, tail) = replay(Profile::Loss2, 99, decisions, count);
    let batch_drops = tail.iter().filter(|a| a.is_drop()).count();

    assert_eq!(got, count - batch_drops, "receiver sees batch minus drops");
    assert_eq!(
        stats.dropped,
        decisions - stats.forwarded,
        "loss2: every non-forwarded decision was a drop"
    );
    assert_eq!(stats.duplicated, 0);
    assert!(
        batch_drops > 0,
        "loss2 with seed 99 must drop batch datagrams"
    );
    assert!(
        (0.01..=0.05).contains(&(stats.dropped as f64 / decisions as f64)),
        "loss2 drop rate {} outside band",
        stats.dropped as f64 / decisions as f64
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jitter30_loses_nothing_duplicates_sometimes() -> Result<()> {
    // Seed 42: verified that the batch window contains duplicates, so the
    // assertions below hold deterministically.
    let (listen_port, _forward_port, receiver, quit, handle) =
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

    let decisions = decisions_consumed(&stats);
    assert!(decisions >= count as u64, "window must cover the batch");
    let (_, tail) = replay(Profile::Jitter30, 42, decisions, count);
    let batch_dups = tail.iter().filter(|a| a.duplicates()).count();

    assert_eq!(stats.dropped, 0, "jitter30 never drops");
    assert_eq!(got, count + batch_dups, "batch records at receiver");
    // Every decision delivers its original exactly once (the 1 s quiet wait
    // flushes all <=60 ms delays before shutdown), duplicates add copies.
    assert_eq!(stats.delayed, decisions);
    assert_eq!(stats.forwarded, decisions + stats.duplicated);
    Ok(())
}
