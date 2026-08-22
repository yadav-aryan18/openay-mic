//! OpenAY Mic lossy-network proxy core.
//!
//! The proxy sits between the phone and the desktop receiver and applies a
//! configurable loss profile to the audio stream. The wire protocol is
//! **strictly one-way** — the phone only sends 48 kHz mono audio datagrams
//! and never expects a reply — so the proxy is one-way too: it receives on
//! the `listen` socket and forwards toward `forward`, and it never sends a
//! single byte back to the phone. Any receiver-side recovery jitter (the
//! receiver synthesizing audio to hide missing datagrams) is out of scope
//! for the proxy itself.
//!
//! # Determinism
//!
//! Every profile samples its decisions from the seed-fixable
//! [`SplitMix64`] generator described in [`DecisionEngine`]: the same seed
//! always yields the identical decision sequence for a given arrival order
//! (no wall-clock or OS entropy in the decision path). The default seed
//! is [`DEFAULT_SEED`].
//!
//! # Profiles
//!
//! See [`Profile`] for the exact semantics of `clean`, `loss2`, `burst`
//! (Gilbert–Elliott) and `jitter30` (0–60 ms delay + 1% duplicates).

mod delay;
mod profile;
mod rng;

use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::time::{Instant, MissedTickBehavior, Sleep};

pub use delay::DelayQueue;
pub use profile::{Action, DecisionEngine, Profile};
pub use rng::SplitMix64;

/// Fixed default seed: reproducible decision sequences out of the box.
/// (Hex `A7 B0 09 4A 5E ED A1 1C` — 2024-09-14, an OpenAY launch date.)
pub const DEFAULT_SEED: u64 = 0xA7B0_094A_5EED_A11C;

/// Maximum UDP payload (RFC 768: 65535 bytes minus IP/UDP headers).
const MAX_DATAGRAM: usize = 65_507;

/// How often the periodic statistics line is printed.
const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// Proxy configuration: where to listen, where to forward, and the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyConfig {
    /// Local socket the phone sends its datagrams to.
    pub listen: SocketAddr,
    /// Remote socket datagrams are forwarded to.
    pub forward: SocketAddr,
    /// Loss/delay profile to apply.
    pub profile: Profile,
    /// Seed for the profile's decision engine ([`DEFAULT_SEED`] if unset).
    pub seed: u64,
}

impl ProxyConfig {
    /// Create a config with the default [`DEFAULT_SEED`].
    #[must_use]
    pub fn new(listen: SocketAddr, forward: SocketAddr, profile: Profile) -> Self {
        Self {
            listen,
            forward,
            profile,
            seed: DEFAULT_SEED,
        }
    }

    /// Build with an explicit seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

/// Aggregate forwarding counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProxyStats {
    /// Datagrams actually sent toward `forward` (immediate + delayed +
    /// duplicate copies).
    pub forwarded: u64,
    /// Datagrams dropped by the profile.
    pub dropped: u64,
    /// Immediate duplicate copies sent (jitter30 1% rule).
    pub duplicated: u64,
    /// Datagrams that were scheduled through the delay path.
    pub delayed: u64,
}

/// Run the proxy until `quit` is set.
///
/// Datagrams received on `config.listen` are handled according to
/// `config.profile`: dropped, forwarded immediately, and/or scheduled for
/// delivery through a deadline-ordered [`DelayQueue`]. After every
/// [`STATS_INTERVAL`] a line is printed to stdout:
///
/// ```text
/// PROXY t=<seconds> forwarded=N dropped=D dup=DUP delayed=X
/// ```
///
/// One final line with the same format is printed when the proxy exits.
/// On exit, already-expired delayed datagrams are flushed best-effort;
/// datagrams whose delay has not elapsed yet are dropped.
///
/// The proxy is one-way: nothing is ever sent back to the `listen` socket.
pub async fn run_proxy(config: ProxyConfig, quit: Arc<AtomicBool>) -> Result<ProxyStats> {
    let listen = UdpSocket::bind(config.listen)
        .await
        .with_context(|| format!("bind listen socket {config:?}"))?;
    // A single sender socket can reach any forward address.
    let out = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("bind forward socket")?;

    // Dedicated reader task: it keeps the listen socket drained at
    // near-syscall speed and hands datagrams to the forwarder over a
    // channel. A single select loop would spend microseconds per datagram
    // in RNG decisions, send syscalls and future re-arming; a synchronous
    // blast (worst case on loopback) would overflow the kernel receive
    // buffer during that window (only ~276 tiny datagrams fit the default
    // rcvbuf accounting) and silently drop datagrams that the profiles
    // never got to see. With the reader running concurrently, the forwarder
    // can never starve the socket.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            match listen.recv_from(&mut buf).await {
                Ok((n, _src)) => {
                    if tx.send(buf[..n].to_vec()).await.is_err() {
                        break; // forwarder went away
                    }
                }
                Err(e) => eprintln!("openay-proxy: recv error: {e}"),
            }
        }
    });

    let mut engine = DecisionEngine::new(config.profile, config.seed);
    let mut queue: DelayQueue<Vec<u8>> = DelayQueue::new();
    let mut stats = ProxyStats::default();
    let started = Instant::now();

    let mut ticker = tokio::time::interval(STATS_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await; // drop the immediate first tick (no t=0 line)

    // Sleep armed on the head of the delay queue; re-armed whenever the
    // earliest deadline changes (i.e. whenever a datagram is scheduled).
    let mut sched: Option<Pin<Box<Sleep>>> = None;

    loop {
        if sched.is_none() {
            if let Some(deadline) = queue.deadline() {
                sched = Some(Box::pin(tokio::time::sleep_until(deadline)));
            }
        }
        tokio::select! {
            received = rx.recv() => {
                let Some(payload) = received else { break }; // reader died
                // A newly scheduled item may have an earlier deadline than
                // the currently parked sleep; re-arm on the next iteration.
                sched = None;
                match engine.decide() {
                    Action::ForwardImmediate => {
                        out.send_to(&payload, config.forward)
                            .await
                            .context("forward datagram")?;
                        stats.forwarded += 1;
                    }
                    Action::ForwardDelayed(delay) => {
                        queue.push(Instant::now() + delay, payload);
                        stats.delayed += 1;
                    }
                    Action::ForwardImmediatePlusDelayed(delay) => {
                        // Duplicate goes out immediately, original via delay.
                        out.send_to(&payload, config.forward)
                            .await
                            .context("forward duplicate")?;
                        stats.forwarded += 1;
                        stats.duplicated += 1;
                        queue.push(Instant::now() + delay, payload);
                        stats.delayed += 1;
                    }
                    Action::Drop => stats.dropped += 1,
                }
            }
            _ = async {
                match sched.as_mut() {
                    Some(sleep) => sleep.as_mut().await,
                    // No scheduled items: this branch never fires.
                    None => std::future::pending::<()>().await,
                }
            } => {
                let expired = queue.pop_expired(Instant::now());
                for payload in expired {
                    out.send_to(&payload, config.forward)
                        .await
                        .context("forward delayed datagram")?;
                    stats.forwarded += 1;
                }
                sched = None;
            }
            _ = ticker.tick() => print_stats(&started, stats),
            _ = async {
                let q = quit.clone();
                while !q.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            } => break,
        }
    }

    // Stop the reader; any datagrams it had queued that we never decided on
    // are dropped at shutdown (best effort).
    reader.abort();
    let _ = reader.await;

    // Best-effort shutdown flush of already-expired delayed datagrams.
    for payload in queue.pop_expired(Instant::now()) {
        out.send_to(&payload, config.forward)
            .await
            .context("flush delayed datagram on shutdown")?;
        stats.forwarded += 1;
    }
    print_stats(&started, stats);
    Ok(stats)
}

fn print_stats(started: &Instant, stats: ProxyStats) {
    println!(
        "PROXY t={} forwarded={} dropped={} dup={} delayed={}",
        started.elapsed().as_secs(),
        stats.forwarded,
        stats.dropped,
        stats.duplicated,
        stats.delayed
    );
}
