//! `openay-proxy` — lossy-network proxy CLI for OpenAY Mic.
//!
//! One-way UDP forwarder between the phone and the desktop receiver, with a
//! configurable loss/delay profile (see [`openay_proxy::Profile`]).
//!
//! ```text
//! openay-proxy --listen IP:PORT --forward IP:PORT \
//!               --profile clean|loss2|burst|jitter30 [--seed N]
//! ```
//!
//! Statistics are printed to stdout every 5 s; the final line is printed on
//! shutdown (SIGINT via Ctrl-C, or SIGTERM).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use openay_proxy::{Profile, ProxyConfig, DEFAULT_SEED};

#[derive(Parser, Debug)]
#[command(
    name = "openay-proxy",
    version,
    about = "Lossy-network proxy for OpenAY Mic's one-way UDP audio stream"
)]
struct Cli {
    /// Socket to receive the phone's datagrams on.
    #[arg(long, value_name = "IP:PORT")]
    listen: SocketAddr,

    /// Socket datagrams are forwarded to.
    #[arg(long, value_name = "IP:PORT")]
    forward: SocketAddr,

    /// Loss profile to apply.
    #[arg(long, value_enum, value_name = "clean|loss2|burst|jitter30")]
    profile: CliProfile,

    /// PRNG seed for a reproducible decision sequence.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_SEED)]
    seed: u64,
}

/// CLI-facing profile mirror delimited by `clap::ValueEnum`.
#[derive(ValueEnum, Clone, Copy, Debug)]
enum CliProfile {
    /// Forward everything unchanged.
    Clean,
    /// Drop each datagram independently with p = 0.02.
    Loss2,
    /// Gilbert–Elliott bursty loss (mean ~9% loss, bursts of ~10).
    Burst,
    /// Uniform 0–60 ms extra delay + 1% immediate duplicates.
    Jitter30,
}

impl From<CliProfile> for Profile {
    fn from(p: CliProfile) -> Self {
        match p {
            CliProfile::Clean => Profile::Clean,
            CliProfile::Loss2 => Profile::Loss2,
            CliProfile::Burst => Profile::Burst,
            CliProfile::Jitter30 => Profile::Jitter30,
        }
    }
}

/// Wait for a shutdown signal (SIGINT via Ctrl-C, or SIGTERM), then set the
/// quit flag so `run_proxy` prints its final statistics line.
fn spawn_signal_handler(quit: Arc<AtomicBool>) {
    // Note: like most tokio programs, a signal whose disposition was SIG_IGN
    // at process start (e.g. SIGINT in a `cmd &` background job) is not
    // caught; foreground Ctrl-C works as expected.
    tokio::spawn(async move {
        #[cfg(unix)]
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            Ok(sig) => Some(sig),
            Err(e) => {
                eprintln!("warning: cannot listen for SIGTERM: {e}");
                None
            }
        };
        #[cfg(not(unix))]
        let mut term = None;

        tokio::select! {
            r = tokio::signal::ctrl_c() => {
                if let Err(e) = r {
                    eprintln!("warning: signal handler failed: {e}");
                }
            }
            _ = async {
                #[cfg(unix)]
                if let Some(sig) = term.as_mut() {
                    sig.recv().await;
                }
                #[cfg(not(unix))]
                std::future::pending::<()>().await;
            } => {}
        }
        quit.store(true, Ordering::Relaxed);
    });
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = ProxyConfig {
        listen: cli.listen,
        forward: cli.forward,
        profile: cli.profile.into(),
        seed: cli.seed,
    };

    let quit = Arc::new(AtomicBool::new(false));
    spawn_signal_handler(quit.clone());

    match openay_proxy::run_proxy(config, quit).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
