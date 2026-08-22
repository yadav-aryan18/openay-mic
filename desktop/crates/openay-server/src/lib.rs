//! OpenAY Mic desktop receiver engine (library form).
//!
//! The `openay-server` CLI and the `openay-gui` console both drive the same
//! engine: a network receive pipeline (UDP/TCP) that decodes OpenAY audio
//! packets into `f32` samples, feeds a lock-free jitter buffer, and — with
//! the `pipewire` feature — exposes the audio as a PipeWire virtual
//! microphone source node (`openay_mic`).
//!
//! The receive/jitter pipeline works without PipeWire; the virtual source is
//! compiled in with `--features pipewire`.
//!
//! # Architecture
//!
//! [`spawn_engine`] starts a dedicated engine thread (with its own tokio
//! runtime) and returns an [`EngineHandle`]. **The handle is cold at
//! creation**: no sockets are bound and nothing is received until an
//! [`EngineCommand::Start`] is sent. The optional config passed to
//! [`spawn_engine`] is only recorded as the defaults for the first `Start`
//! (it also feeds the standby [`EngineStatus`] display); the caller starts
//! the engine explicitly:
//!
//! ```ignore
//! let handle = spawn_engine(Some(config));
//! handle.cmd().send(EngineCommand::Start(config)).await?;
//! ```
//!
//! Commands ([`Start`] / [`Stop`]) are sent over the
//! handle's channel; status is read as a cheap snapshot
//! ([`EngineHandle::status`]). The engine can be stopped and restarted with
//! a different config without creating a new handle, which is what settings
//! changes in the GUI rely on. Each `Start` begins a fresh run: while
//! stopped, the snapshot reports `running == false` with every transport
//! counter, `fill_ms`, `level_peak`, and `uptime_secs` at zero; the run's
//! final numbers survive only in the canonical stats line (see
//! [`EngineHandle::take_stats_line`]).
//!
//! [`Start`]: EngineCommand::Start
//! [`Stop`]: EngineCommand::Stop

mod ingest;
#[cfg(feature = "pipewire")]
mod pw;

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use openay_jitter::{DepthController, DepthParams, JitterBuffer, RealClock};
use openay_protocol::PayloadType;
use tokio::sync::mpsc;

use crate::ingest::Ingest;

/// Samples per second (protocol-fixed: 48 kHz mono).
pub const SAMPLE_RATE: usize = 48_000;
/// Interval between stats lines printed to stdout.
const STATS_INTERVAL: Duration = Duration::from_secs(5);
/// Minimum time between `UNDERRUN episodes=...` stderr lines (rate limit).
const UNDERRUN_LOG_INTERVAL: Duration = Duration::from_secs(5);
/// Tick period of the per-pipeline depth controller task.
const DEPTH_TICK: Duration = Duration::from_millis(200);
/// How fast the depth controller task polls the pipeline quit flag.
const QUIT_POLL: Duration = Duration::from_millis(25);
/// Largest possible wire datagram: 6-byte header + 65535-byte payload.
const MAX_DATAGRAM: usize = 65541;
/// Jitter buffer capacity in ms of audio (matches the CLI's
/// `--capacity-ms` default; fixed for library users).
const DEFAULT_CAPACITY_MS: f32 = 100.0;
/// The RT callback folds max |sample| into this quantum; `level_peak` is
/// this raw value divided by `PEAK_SCALE` (so 1.0 == full scale).
const PEAK_SCALE: u32 = 65_535;

pub use openay_jitter::{MAX_PREBUFFER_MS, MIN_PREBUFFER_MS};

/// Transport protocol the engine listens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    #[default]
    Udp,
    Tcp,
}

impl Transport {
    /// Lowercase name, as used in CLI output and the GUI status line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::Udp => "udp",
            Transport::Tcp => "tcp",
        }
    }
}

/// Which payload types the engine accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodecMode {
    /// Accept either PCM or Opus payloads, per packet.
    #[default]
    Auto,
    /// Only raw PCM payloads are accepted.
    Pcm,
    /// Only Opus payloads are accepted.
    Opus,
}

impl CodecMode {
    /// Uppercase name for the GUI status line ("AUTO"/"PCM"/"OPUS").
    pub const fn as_str(self) -> &'static str {
        match self {
            CodecMode::Auto => "auto",
            CodecMode::Pcm => "pcm",
            CodecMode::Opus => "opus",
        }
    }

    /// The payload type this mode restricts to (`None` in Auto).
    pub const fn only(self) -> Option<PayloadType> {
        match self {
            CodecMode::Auto => None,
            CodecMode::Pcm => Some(PayloadType::Pcm),
            CodecMode::Opus => Some(PayloadType::Opus),
        }
    }
}

/// Configuration for the receive pipeline. See [`EngineConfig::validated`]
/// for the normalization rules (port range, target clamp).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    pub transport: Transport,
    pub bind: IpAddr,
    pub port: u16,
    pub codec: CodecMode,
    /// Prebuffer target in ms before streaming starts
    /// (clamped to `[MIN_PREBUFFER_MS, MAX_PREBUFFER_MS]` = 5..=20).
    pub target_ms: f32,
    /// Jitter buffer capacity in ms of audio.
    pub capacity_ms: f32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            transport: Transport::Udp,
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 41_700,
            codec: CodecMode::Auto,
            target_ms: 10.0,
            capacity_ms: DEFAULT_CAPACITY_MS,
        }
    }
}

/// Errors from [`EngineConfig::validated`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// Port 0 is not a valid listen port for a session.
    InvalidPort(u16),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::InvalidPort(port) => {
                write!(f, "invalid port {port}: must be in 1..=65535")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl EngineConfig {
    /// Validate and normalize a config:
    ///
    /// - `port` must be `1..=65535` (otherwise `Err(InvalidPort)`);
    /// - `target_ms` is clamped into `[MIN_PREBUFFER_MS, MAX_PREBUFFER_MS]`.
    pub fn validated(self) -> Result<EngineConfig, ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::InvalidPort(self.port));
        }
        Ok(EngineConfig {
            target_ms: self.target_ms.clamp(MIN_PREBUFFER_MS, MAX_PREBUFFER_MS),
            ..self
        })
    }
}

/// Snapshot of the engine's live state.
///
/// Per-run semantics: all fields describe the *current* run only. When
/// [`running`](EngineStatus::running) is `false` — never started, stopped,
/// or failed to start — every transport counter (`received`, `lost`, `dup`,
/// `ooo`, `malformed`, `overruns`, `underruns`), `fill_ms`, `level_peak`,
/// and `uptime_secs` reads zero. [`take_stats_line`](EngineHandle::take_stats_line)
/// is the only way to obtain a run's final numbers after it has stopped.
///
/// [`effective_target_ms`](EngineStatus::effective_target_ms) is the
/// exception: while stopped it reports the configured `target_ms` (the
/// standby/pre-run depth), while running it reports the live adaptive value
/// written by the depth controller task — an underrun raises it +2 ms toward
/// [`MAX_PREBUFFER_MS`], each fully elapsed clean window (60 s) lowers it 1 ms
/// back toward the configured base.
///
/// [`level_peak`](EngineStatus::level_peak) is the peak capture level over
/// the interval since the *previous* snapshot, in `0.0..=1.0` — each
/// `status()` call consumes the interval (same semantics as the Android
/// side). Without the `pipewire` feature no audio is consumed, so the level
/// stays `0.0` (documented; asserted by the headless smoke test).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineStatus {
    pub running: bool,
    pub transport: Transport,
    pub bind: IpAddr,
    pub port: u16,
    pub codec: CodecMode,
    pub received: u64,
    pub lost: u64,
    pub dup: u64,
    pub ooo: u64,
    pub malformed: u64,
    pub overruns: u64,
    pub underruns: u64,
    /// Jitter buffer occupancy in ms of audio.
    pub fill_ms: f32,
    /// Peak capture level since the last snapshot, `0.0..=1.0`
    /// (consumed on read; always `0.0` while stopped).
    pub level_peak: f32,
    /// Seconds since the last `Start` (0 when stopped).
    pub uptime_secs: u64,
    /// Live adaptive prebuffer depth in ms: the configured `target_ms` while
    /// stopped, the depth controller's readjusted value while running
    /// (see the struct docs for the rise/decay law).
    pub effective_target_ms: f32,
}

/// Commands sent to the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineCommand {
    /// Start (or restart with) the given config. A running pipeline is
    /// stopped first.
    Start(EngineConfig),
    /// Stop the pipeline; the handle stays valid for a later `Start`.
    Stop,
}

/// Handle to a running engine. Cheap to clone; drops do not stop the engine
/// (only closing every clone's command sender would, once the handle is
/// dropped everywhere).
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: mpsc::Sender<EngineCommand>,
    state: Arc<EngineState>,
}

impl EngineHandle {
    /// A snapshot of the current engine state. The level peak is consumed
    /// by this call (see [`EngineStatus::level_peak`]).
    ///
    /// Per-run semantics: when `running == false` every transport counter
    /// (`received`/`lost`/`dup`/`ooo`/`malformed`/`overruns`/`underruns`),
    /// `fill_ms`, `level_peak`, and `uptime_secs` read zero — the snapshot
    /// describes the current run only, never lifetime totals.
    /// [`effective_target_ms`](EngineStatus::effective_target_ms) reads the
    /// configured `target_ms` while stopped (the standby depth), and the
    /// depth controller's live value while running. Use
    /// [`EngineHandle::take_stats_line`] for a run's final numbers after
    /// `Stop`.
    pub fn status(&self) -> EngineStatus {
        let running = self.state.running.load(Ordering::Relaxed);
        let cfg = self
            .state
            .config
            .lock()
            .expect("engine config mutex poisoned")
            .unwrap_or_default();
        // Always consume the peak accumulator (it is stale once stopped);
        // only report it while running.
        let level_peak = self.state.peak.swap(0, Ordering::Relaxed) as f32 / PEAK_SCALE as f32;

        let mut s = EngineStatus {
            running,
            transport: cfg.transport,
            bind: cfg.bind,
            port: cfg.port,
            codec: cfg.codec,
            received: 0,
            lost: 0,
            dup: 0,
            ooo: 0,
            malformed: 0,
            overruns: 0,
            underruns: 0,
            fill_ms: 0.0,
            level_peak: if running { level_peak } else { 0.0 },
            uptime_secs: 0,
            effective_target_ms: cfg.target_ms,
        };

        if running {
            // The live adaptive depth: what the RT latch actually waits for.
            s.effective_target_ms = self.state.effective_target.load(Ordering::Acquire) as f32
                / SAMPLE_RATE as f32
                * 1000.0;
            if let Some(jitter) = self
                .state
                .jitter
                .lock()
                .expect("engine jitter mutex poisoned")
                .as_ref()
            {
                s.fill_ms = jitter.available() as f32 / SAMPLE_RATE as f32 * 1000.0;
                s.overruns = jitter.overruns();
                s.underruns = jitter.underruns();
            }
            if let Some(ingest) = self
                .state
                .ingest
                .lock()
                .expect("engine ingest mutex poisoned")
                .as_ref()
            {
                let g = ingest.lock().expect("ingest mutex poisoned");
                s.received = g.received;
                s.lost = g.lost;
                s.dup = g.duplicate;
                s.ooo = g.out_of_order;
                s.malformed = g.malformed;
            }
            if let Some(t0) = self
                .state
                .started_at
                .lock()
                .expect("engine uptime mutex poisoned")
                .as_ref()
            {
                s.uptime_secs = t0.elapsed().as_secs();
            }
        }
        s
    }

    /// A clone of the command channel (send [`Start`] / [`Stop`] to drive
    /// the engine).
    ///
    /// [`Start`]: EngineCommand::Start
    /// [`Stop`]: EngineCommand::Stop
    pub fn cmd(&self) -> mpsc::Sender<EngineCommand> {
        self.cmd_tx.clone()
    }

    /// The most recent fatal error (bind failure, PipeWire setup failure,
    /// network task panic), cleared when the next `Start` begins — a failed
    /// start immediately sets a fresh error in its place.
    pub fn last_error(&self) -> Option<String> {
        self.state
            .last_error
            .lock()
            .expect("error mutex poisoned")
            .clone()
    }

    /// The canonical `SRV transport=...` stats line of the last *stopped*
    /// run (printed once by the CLI). `take`-ing it consumes it; it is also
    /// cleared at the beginning of every `Start`, so a failed start never
    /// surfaces a stale line from an earlier run.
    pub fn take_stats_line(&self) -> Option<String> {
        self.state
            .last_stats_line
            .lock()
            .expect("stats line mutex poisoned")
            .take()
    }

    /// The live jitter buffer of the current run, for tests and diagnostics.
    ///
    /// This is a test-support entry point, not part of the supported status
    /// API: the engine's SPSC contract allows *one* consumer, and in a
    /// headless (`pipewire` off) build the network task is the only owner of
    /// the producer side, so a test can take the consumer role — pop at the
    /// realtime pace and call [`JitterBuffer::note_underrun`] on dry pops,
    /// exactly like the PipeWire RT callback does. With the `pipewire`
    /// feature the RT callback is that consumer; two consumers would violate
    /// the ring protocol. `None` while stopped.
    ///
    /// Compiled out under the `pipewire` feature: there the RT callback is
    /// the consumer, and a second consumer popping from this handle would be
    /// a data race on the ring's interior mutability (not merely a logic
    /// bug), so misuse must not typecheck.
    #[doc(hidden)]
    #[cfg(not(feature = "pipewire"))]
    pub fn jitter_for_test(&self) -> Option<Arc<JitterBuffer>> {
        self.state
            .jitter
            .lock()
            .expect("engine jitter mutex poisoned")
            .clone()
    }
}

/// Shared engine state, readable from any thread.
struct EngineState {
    running: AtomicBool,
    /// The engine's current/standby config (last successful `Start`, or the
    /// config passed to [`spawn_engine`] before the first start). Reported by
    /// `status()` even while stopped, so callers can render the standby
    /// state with the intended transport/bind/port/codec.
    config: Mutex<Option<EngineConfig>>,
    jitter: Mutex<Option<Arc<JitterBuffer>>>,
    ingest: Mutex<Option<Arc<Mutex<Ingest>>>>,
    started_at: Mutex<Option<Instant>>,
    last_error: Mutex<Option<String>>,
    last_stats_line: Mutex<Option<String>>,
    /// Max |sample| over the current interval, scaled to `0..=65535`.
    /// Written by the PipeWire RT callback (strict-max CAS), read by
    /// exchanging (consumed per snapshot). Reset to 0 on every `Start`/`Stop`
    /// so a stopped engine never reports a stale peak.
    peak: Arc<AtomicU32>,
    /// Live adaptive prebuffer depth in *samples*
    /// (`ceil(target_ms * 48_000 / 1000)`). Written by the per-pipeline depth
    /// controller task (and seeded at every `Start` with the configured
    /// `target_ms`); read by the RT latch and by `status()`. Headless builds
    /// keep it too so `status().effective_target_ms` is accurate without
    /// PipeWire.
    effective_target: Arc<AtomicU32>,
    /// Timing knobs for the per-pipeline depth controller (set by
    /// [`spawn_engine_tuned`], default [`DepthParams`] otherwise).
    depth_params: DepthParams,
}

impl Default for EngineState {
    fn default() -> Self {
        EngineState {
            running: AtomicBool::new(false),
            config: Mutex::new(None),
            jitter: Mutex::new(None),
            ingest: Mutex::new(None),
            started_at: Mutex::new(None),
            last_error: Mutex::new(None),
            last_stats_line: Mutex::new(None),
            peak: Arc::new(AtomicU32::new(0)),
            effective_target: Arc::new(AtomicU32::new(0)),
            depth_params: DepthParams::default(),
        }
    }
}

impl EngineState {
    fn set_error(&self, msg: String) {
        *self.last_error.lock().expect("error mutex poisoned") = Some(msg);
    }

    fn clear_error(&self) {
        *self.last_error.lock().expect("error mutex poisoned") = None;
    }

    fn clear_last_stats_line(&self) {
        *self
            .last_stats_line
            .lock()
            .expect("stats line mutex poisoned") = None;
    }

    fn mark_running(&self) {
        self.running.store(true, Ordering::Relaxed);
    }

    fn mark_stopped(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Spawn the engine on a dedicated thread (with its own tokio runtime) and
/// return a handle. The handle starts **cold**: no sockets are bound and no
/// pipeline runs until an [`EngineCommand::Start`] is sent.
///
/// `initial` (if `Some`) is recorded as the *default configuration* for the
/// standby [`EngineStatus`] display (transport, bind, port, codec) and as the
/// config the engine considers current until the first `Start` overwrites
/// it. It is deliberately **not** started — the caller must send
/// [`EngineCommand::Start`] to begin receiving. The CLI follows the
/// canonical pattern:
///
/// ```ignore
/// let handle = spawn_engine(Some(config));
/// handle.cmd().send(EngineCommand::Start(config)).await?;
/// ```
///
/// The handle is cheap to clone. Dropping every clone of the handle closes
/// the command channel and lets the engine wind down after the running
/// pipeline finishes.
pub fn spawn_engine(initial: Option<EngineConfig>) -> EngineHandle {
    spawn_engine_tuned(initial, DepthParams::default())
}

/// Like [`spawn_engine`], but with explicit [`DepthParams`] for the jitter
/// depth controller (rise per underrun, clean-window length, decay step).
///
/// Intended for tests and scenario validation: [`DepthParams::default`] is
/// the production policy (+2 ms per underrun, −1 ms per fully elapsed 60 s
/// clean window), and shrinking the 60 s window lets a validation scenario
/// watch a full rise/decay cycle in seconds. Everything else — cold-start
/// contract, command/status semantics, per-`Start` reset — is identical to
/// [`spawn_engine`].
pub fn spawn_engine_tuned(
    initial: Option<EngineConfig>,
    depth_params: DepthParams,
) -> EngineHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let state = Arc::new(EngineState {
        depth_params,
        ..EngineState::default()
    });
    // The config is only "defaults for the first Start": publish it for the
    // standby status display, but do NOT start a pipeline.
    if let Some(cfg) = initial {
        *state.config.lock().expect("engine config mutex poisoned") = Some(cfg);
    }
    let thread_state = state.clone();
    std::thread::Builder::new()
        .name("openay-engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("building the engine tokio runtime");
            rt.block_on(engine_main(cmd_rx, thread_state));
        })
        .expect("spawning the engine thread");
    EngineHandle { cmd_tx, state }
}

/// The engine's command loop: owns the lifecycle of the pipeline.
///
/// The engine is cold on entry: nothing runs until a `Start` command
/// arrives. A pipeline that dies on its own (bind failure, PipeWire setup
/// failure) is reaped on the periodic tick, which also guarantees
/// `status().running == false` once the network task has exited.
async fn engine_main(mut cmd_rx: mpsc::Receiver<EngineCommand>, state: Arc<EngineState>) {
    let mut pipeline: Option<Pipeline> = None;

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(EngineCommand::Start(cfg)) => {
                        if let Some(p) = pipeline.take() {
                            stop_pipeline(p, &state).await;
                        }
                        match start_pipeline(cfg, &state).await {
                            Ok(p) => pipeline = Some(p),
                            Err(e) => {
                                state.set_error(format!("{e:#}"));
                                state.mark_stopped();
                            }
                        }
                    }
                    Some(EngineCommand::Stop) => {
                        if let Some(p) = pipeline.take() {
                            stop_pipeline(p, &state).await;
                        }
                    }
                    None => break,
                }
            }
            _ = tick.tick() => {
                // Reap a pipeline that ended by itself (PipeWire setup
                // failure, bind failure) so the state reflects reality.
                if pipeline.as_ref().is_some_and(Pipeline::is_done) {
                    let p = pipeline.take().expect("checked above");
                    stop_pipeline(p, &state).await;
                }
            }
        }
    }

    if let Some(p) = pipeline.take() {
        stop_pipeline(p, &state).await;
    }
}

/// A live pipeline: network receive task + optional PipeWire thread + the
/// depth controller task, all over one jitter buffer.
///
/// The `jitter` and `ingest` fields are never read directly (the compiler
/// warns about them) but they are ownership-load-bearing: dropping the
/// `Pipeline` decrements the `Arc` refcounts, keeping the jitter buffer and
/// the ingest state alive for the network task until the pipeline is torn
/// down. `retarget` is aborted on teardown (it also self-exits on `quit`).
#[allow(dead_code)]
struct Pipeline {
    jitter: Arc<JitterBuffer>,
    ingest: Arc<Mutex<Ingest>>,
    quit: Arc<AtomicBool>,
    net_task: tokio::task::JoinHandle<Result<String>>,
    retarget: tokio::task::JoinHandle<()>,
    #[cfg(feature = "pipewire")]
    pw_thread: Option<std::thread::JoinHandle<Result<()>>>,
}

impl Pipeline {
    /// True when the pipeline can no longer serve audio: the quit flag was
    /// set (Stop or PipeWire failure) and both tasks have exited, or the
    /// network task ended on its own (fatal bind/I/O error).
    fn is_done(&self) -> bool {
        let net_done = self.net_task.is_finished();
        #[cfg(feature = "pipewire")]
        let pw_done = self
            .pw_thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished);
        #[cfg(not(feature = "pipewire"))]
        let pw_done = true;
        self.quit.load(Ordering::Relaxed) && net_done && pw_done
            || net_done && !self.quit.load(Ordering::Relaxed)
    }
}

/// Start the pipeline for `config` and publish the shared state.
///
/// A `Start` resets every per-run counter: the jitter buffer and ingest
/// state are created fresh, the level peak accumulator is zeroed, and any
/// leftover stats line from a previous run is cleared (a failed `Start`
/// therefore never surfaces stale numbers).
async fn start_pipeline(config: EngineConfig, state: &Arc<EngineState>) -> Result<Pipeline> {
    let config = config.validated().map_err(|e| anyhow::anyhow!(e))?;
    state.clear_error();
    state.clear_last_stats_line();
    state.peak.store(0, Ordering::Relaxed);

    let capacity_samples = (config.capacity_ms * SAMPLE_RATE as f32 / 1000.0) as usize;
    let jitter = Arc::new(JitterBuffer::new(capacity_samples));
    let quit = Arc::new(AtomicBool::new(false));
    let ingest = Arc::new(Mutex::new(Ingest::new(jitter.clone(), config.codec.only())));

    // Seed the adaptive depth with the configured target and share it with
    // the RT latch (pipewire), the depth controller task, and `status()`.
    // Release pairs with status()'s Acquire load: a snapshot that already
    // observes `running == true` (published later by mark_running) can never
    // see the *previous* run's stale depth here.
    let target_samples =
        (config.target_ms * SAMPLE_RATE as f32 / 1000.0).ceil() as u32;
    state.effective_target.store(target_samples, Ordering::Release);

    #[cfg(feature = "pipewire")]
    let pw_thread = {
        let streaming = Arc::new(AtomicBool::new(false));
        let (setup_tx, setup_rx) = std::sync::mpsc::channel();
        let shared = pw::PwShared {
            jitter: jitter.clone(),
            streaming,
            quit: quit.clone(),
            target_samples: state.effective_target.clone(),
            peak_level: state.peak.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("openay-pipewire".into())
            .spawn(move || pw::run_pipewire(shared, setup_tx))
            .context("spawning PipeWire thread")?;
        // A PipeWire setup failure ends the whole pipeline: record the
        // error and set the quit flag (the engine reaps it on its next tick).
        spawn_pw_monitor(quit.clone(), setup_rx, state);
        Some(thread)
    };

    #[cfg(not(feature = "pipewire"))]
    eprintln!("openay-server: built without PipeWire support — network+jitter only");

    let net = tokio::spawn(run_network(
        config,
        ingest.clone(),
        jitter.clone(),
        state.effective_target.clone(),
        quit.clone(),
    ));
    let retarget = spawn_depth_controller(
        jitter.clone(),
        state.effective_target.clone(),
        config.target_ms,
        state.depth_params,
        quit.clone(),
    );

    *state.config.lock().expect("engine config mutex poisoned") = Some(config);
    *state.jitter.lock().expect("engine jitter mutex poisoned") = Some(jitter.clone());
    *state.ingest.lock().expect("engine ingest mutex poisoned") = Some(ingest.clone());
    *state
        .started_at
        .lock()
        .expect("engine uptime mutex poisoned") = Some(Instant::now());
    state.mark_running();

    Ok(Pipeline {
        jitter,
        ingest,
        quit,
        net_task: net,
        retarget,
        #[cfg(feature = "pipewire")]
        pw_thread,
    })
}

/// Per-pipeline adaptive depth controller task.
///
/// Ticks every [`DEPTH_TICK`] (200 ms): if the jitter underrun counter
/// increased since the last poll, the controller records an underrun
/// (+`rise_ms` toward the ceiling); otherwise the running tick earns decay
/// steps per fully elapsed clean window. The resulting effective target in
/// milliseconds is stored into `effective_target` as
/// `ceil(target_ms * 48_000 / 1000)` samples — the value the RT latch and
/// `status()` read. Exits promptly when `quit` is set (Stop / PipeWire
/// failure); the base depth and the timings come from the pipeline config and
/// [`EngineState::depth_params`].
fn spawn_depth_controller(
    jitter: Arc<JitterBuffer>,
    effective_target: Arc<AtomicU32>,
    base_ms: f32,
    params: DepthParams,
    quit: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut depth = DepthController::with_params(RealClock, base_ms, params);
        // The counter starts at 0 for every fresh run (Start resets the
        // buffer); only underruns observed *after* this task starts count.
        let mut last_underruns = jitter.underruns();
        let mut tick = tokio::time::interval(DEPTH_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await; // discard the immediate first tick
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let underruns = jitter.underruns();
                    if underruns > last_underruns {
                        depth.on_underrun();
                        last_underruns = underruns;
                    }
                    let target_ms = depth.on_tick(true);
                    effective_target.store(
                        (target_ms * SAMPLE_RATE as f32 / 1000.0).ceil() as u32,
                        Ordering::Relaxed,
                    );
                }
                _ = async {
                    while !quit.load(Ordering::Relaxed) {
                        tokio::time::sleep(QUIT_POLL).await;
                    }
                } => break,
            }
        }
    })
}

/// Watch the PipeWire setup channel. A setup failure (or an unexpected
/// thread exit) stops the pipeline and records the error. A normal shutdown
/// (Stop sets `quit` first) is not an error and is ignored.
#[cfg(feature = "pipewire")]
fn spawn_pw_monitor(
    quit: Arc<AtomicBool>,
    setup_rx: std::sync::mpsc::Receiver<Result<(), String>>,
    state: &Arc<EngineState>,
) {
    let state = state.clone();
    tokio::spawn(async move {
        let recv = tokio::task::spawn_blocking(move || setup_rx.recv()).await;
        // Normal shutdown: `quit` was set by Stop; the channel disconnecting
        // as the thread exits is expected, not an error.
        if quit.load(Ordering::Relaxed) {
            return;
        }
        let msg = match recv {
            Ok(Ok(Err(e))) => format!("PipeWire setup failed: {e}"),
            Ok(Ok(Ok(()))) => "PipeWire thread exited unexpectedly".to_string(),
            Ok(Err(_)) | Err(_) => "PipeWire setup channel closed unexpectedly".to_string(),
        };
        state.set_error(msg);
        quit.store(true, Ordering::Relaxed);
    });
}

/// Tear the pipeline down: signal quit, await the network task (which
/// returns the final stats line), reap the PipeWire thread, and reset the
/// shared state so `status()` reports `running == false` with zeroed
/// counters. The run's final numbers survive only in the stats line
/// (retrievable via [`EngineHandle::take_stats_line`]).
async fn stop_pipeline(p: Pipeline, state: &Arc<EngineState>) {
    p.quit.store(true, Ordering::Relaxed);
    // The depth controller self-exits on `quit`; abort it so teardown never
    // waits on a missed wakeup (its result is uninteresting).
    p.retarget.abort();
    let _ = p.retarget.await;

    let final_line = match p.net_task.await {
        Ok(Ok(line)) => Some(line),
        Ok(Err(e)) => {
            state.set_error(format!("{e:#}"));
            None
        }
        Err(e) => {
            state.set_error(format!("network task panicked: {e}"));
            None
        }
    };

    #[cfg(feature = "pipewire")]
    if let Some(thread) = p.pw_thread {
        // Give the loop up to ~3 s to wind down (it polls the quit flag
        // every 50 ms), then reap the thread.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if thread.is_finished() {
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
        match thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("openay-server: PipeWire error: {e:#}"),
            Err(_) => eprintln!("openay-server: PipeWire thread panicked"),
        }
    }

    // The canonical stats line survives the stop; the snapshot counters do
    // not (see the per-run semantics on `EngineStatus`).
    if let Some(line) = final_line {
        *state
            .last_stats_line
            .lock()
            .expect("stats line mutex poisoned") = Some(line);
    }
    state.peak.store(0, Ordering::Relaxed);
    *state.jitter.lock().expect("engine jitter mutex poisoned") = None;
    *state.ingest.lock().expect("engine ingest mutex poisoned") = None;
    *state
        .started_at
        .lock()
        .expect("engine uptime mutex poisoned") = None;
    state.mark_stopped();
}

/// Run the receive pipeline until `quit` is set, then return the final
/// stats line.
async fn run_network(
    config: EngineConfig,
    ingest: Arc<Mutex<Ingest>>,
    jitter: Arc<JitterBuffer>,
    effective_target: Arc<AtomicU32>,
    quit: Arc<AtomicBool>,
) -> Result<String> {
    match config.transport {
        Transport::Udp => udp_loop(&config, ingest, jitter, effective_target, quit).await,
        Transport::Tcp => tcp_loop(&config, ingest, jitter, effective_target, quit).await,
    }
}

/// UDP receive loop: one datagram == one packet (protocol spec). Malformed
/// datagrams are dropped and counted, never fatal.
async fn udp_loop(
    config: &EngineConfig,
    ingest: Arc<Mutex<Ingest>>,
    jitter: Arc<JitterBuffer>,
    effective_target: Arc<AtomicU32>,
    quit: Arc<AtomicBool>,
) -> Result<String> {
    let addr = SocketAddr::new(config.bind, config.port);
    let socket = tokio::net::UdpSocket::bind(addr)
        .await
        .with_context(|| format!("binding UDP {addr}"))?;
    eprintln!("openay-server: UDP listening on {addr}");

    let mut buf = [0u8; MAX_DATAGRAM];
    let mut last_stats = Instant::now();
    let mut last_underruns = jitter.underruns();
    let mut last_underrun_log: Option<Instant> = None;
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
        if let Some(line) =
            underrun_episode_line(&jitter, &effective_target, &mut last_underruns, &mut last_underrun_log)
        {
            eprintln!("{line}");
        }
        if last_stats.elapsed() >= STATS_INTERVAL {
            println!("{}", stats_line(Transport::Udp, &ingest, &jitter));
            last_stats = Instant::now();
        }
    }
    Ok(stats_line(Transport::Udp, &ingest, &jitter))
}

/// TCP receive loop: back-to-back framed packets via `TcpPacketStream`; each
/// accepted connection is handled by its own task, so one stalled client
/// cannot block ingestion from others.
async fn tcp_loop(
    config: &EngineConfig,
    ingest: Arc<Mutex<Ingest>>,
    jitter: Arc<JitterBuffer>,
    effective_target: Arc<AtomicU32>,
    quit: Arc<AtomicBool>,
) -> Result<String> {
    let addr = SocketAddr::new(config.bind, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding TCP {addr}"))?;
    eprintln!("openay-server: TCP listening on {addr}");

    let mut last_stats = Instant::now();
    let mut last_underruns = jitter.underruns();
    let mut last_underrun_log: Option<Instant> = None;
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
        if let Some(line) =
            underrun_episode_line(&jitter, &effective_target, &mut last_underruns, &mut last_underrun_log)
        {
            eprintln!("{line}");
        }
        if last_stats.elapsed() >= STATS_INTERVAL {
            println!("{}", stats_line(Transport::Tcp, &ingest, &jitter));
            last_stats = Instant::now();
        }
    }
    Ok(stats_line(Transport::Tcp, &ingest, &jitter))
}

/// Watch the underrun counter on every 200 ms wake; when it increased,
/// return a rate-limited stderr line:
///
/// ```text
/// openay-server: UNDERRUN episodes=+K total=U effective_target_ms=T fill_ms=F
/// ```
///
/// `K` is the increase since the last wake, `U` the run total, `T` the live
/// adaptive depth (the depth controller's current target), and `F` the
/// current jitter fill. At most one line per [`UNDERRUN_LOG_INTERVAL`] (5 s),
/// so a burst of underruns cannot spam the console; the counter however is
/// always advanced so the next increase episode is measured from it.
/// Returns `None` when there is nothing to print (no increase, or the
/// 5 s throttle is active).
fn underrun_episode_line(
    jitter: &JitterBuffer,
    effective_target: &AtomicU32,
    last_underruns: &mut u64,
    last_log: &mut Option<Instant>,
) -> Option<String> {
    let underruns = jitter.underruns();
    if underruns <= *last_underruns {
        return None;
    }
    let episodes = underruns - *last_underruns;
    *last_underruns = underruns;
    let now = Instant::now();
    if last_log.is_some_and(|t| now.duration_since(t) < UNDERRUN_LOG_INTERVAL) {
        return None;
    }
    *last_log = Some(now);
    let fill_ms = jitter.available() as f32 / SAMPLE_RATE as f32 * 1000.0;
    let target_ms = effective_target.load(Ordering::Relaxed) as f32 / SAMPLE_RATE as f32 * 1000.0;
    Some(format!(
        "openay-server: UNDERRUN episodes=+{episodes} total={underruns} \
         effective_target_ms={target_ms:.1} fill_ms={fill_ms:.1}"
    ))
}

/// The canonical server stats line, printed every 5 s and once at shutdown:
/// `SRV transport=<t> received=<n> lost=<n> dup=<d> ooo=<o> malformed=<m>
/// overruns=<r> underruns=<u> fill_ms=<F.1>`
fn stats_line(transport: Transport, ingest: &Mutex<Ingest>, jitter: &JitterBuffer) -> String {
    let g = ingest.lock().expect("ingest mutex poisoned");
    let fill_ms = jitter.available() as f32 / SAMPLE_RATE as f32 * 1000.0;
    format!(
        "SRV transport={} received={} lost={} dup={} ooo={} malformed={} \
         overruns={} underruns={} fill_ms={fill_ms:.1}",
        transport.as_str(),
        g.received,
        g.lost,
        g.duplicate,
        g.out_of_order,
        g.malformed,
        jitter.overruns(),
        jitter.underruns(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_rejects_port_zero() {
        let cfg = EngineConfig {
            port: 0,
            ..EngineConfig::default()
        };
        assert_eq!(cfg.validated(), Err(ConfigError::InvalidPort(0)));
    }

    #[test]
    fn validated_accepts_port_range() {
        for port in [1u16, 1024, 41_700, 65_535] {
            let cfg = EngineConfig {
                port,
                ..EngineConfig::default()
            };
            assert_eq!(cfg.validated().unwrap().port, port);
        }
    }

    #[test]
    fn validated_clamps_target_ms() {
        for (input, expected) in [
            (3.0, MIN_PREBUFFER_MS),
            (10.0, 10.0),
            (500.0, MAX_PREBUFFER_MS),
            (-1.0, MIN_PREBUFFER_MS),
        ] {
            let cfg = EngineConfig {
                target_ms: input,
                ..EngineConfig::default()
            };
            assert_eq!(cfg.validated().unwrap().target_ms, expected);
        }
    }

    #[test]
    fn validated_preserves_other_fields() {
        let cfg = EngineConfig {
            transport: Transport::Tcp,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 4321,
            codec: CodecMode::Pcm,
            target_ms: 17.0,
            capacity_ms: 200.0,
        };
        let v = cfg.validated().unwrap();
        assert_eq!(v, cfg, "already-valid config passes through unchanged");
    }

    #[test]
    fn codec_mode_round_trip_helpers() {
        assert_eq!(CodecMode::Auto.only(), None);
        assert_eq!(CodecMode::Pcm.only(), Some(PayloadType::Pcm));
        assert_eq!(CodecMode::Opus.only(), Some(PayloadType::Opus));
        assert_eq!(Transport::Udp.as_str(), "udp");
        assert_eq!(Transport::Tcp.as_str(), "tcp");
        assert_eq!(CodecMode::Auto.as_str(), "auto");
        assert_eq!(CodecMode::Pcm.as_str(), "pcm");
        assert_eq!(CodecMode::Opus.as_str(), "opus");
    }

    #[test]
    fn default_config_matches_cli_defaults() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.transport, Transport::Udp);
        assert_eq!(cfg.bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(cfg.port, 41_700);
        assert_eq!(cfg.codec, CodecMode::Auto);
        assert_eq!(cfg.target_ms, 10.0);
        assert_eq!(cfg.capacity_ms, 100.0);
        assert!(cfg.validated().is_ok());
    }

    /// The underrun episode line: exact format, only on an increase, and
    /// rate-limited to one per 5 s (the counter still advances).
    #[test]
    fn underrun_episode_line_format_and_throttle() {
        let jb = JitterBuffer::new(1024);
        let target = AtomicU32::new(576); // 12 ms of 48 kHz audio
        let mut last_underruns = jb.underruns();
        let mut last_log = None;
        assert_eq!(
            underrun_episode_line(&jb, &target, &mut last_underruns, &mut last_log),
            None,
            "no increase: no line"
        );
        jb.note_underrun();
        jb.note_underrun();
        let line = underrun_episode_line(&jb, &target, &mut last_underruns, &mut last_log)
            .expect("an increase logs");
        assert_eq!(
            line,
            "openay-server: UNDERRUN episodes=+2 total=2 effective_target_ms=12.0 fill_ms=0.0"
        );
        jb.note_underrun();
        assert_eq!(
            underrun_episode_line(&jb, &target, &mut last_underruns, &mut last_log),
            None,
            "a second increase within 5 s is throttled"
        );
        assert_eq!(last_underruns, 3, "the counter advances even when throttled");
    }

    /// While stopped the snapshot reports the configured target as the
    /// effective depth (standby display), never a stale run value.
    #[test]
    fn status_reports_config_target_as_effective_while_stopped() {
        let cfg = EngineConfig {
            target_ms: 17.0,
            ..EngineConfig::default()
        };
        let handle = spawn_engine(Some(cfg));
        let s = handle.status();
        assert!(!s.running);
        assert_eq!(s.effective_target_ms, 17.0);
    }

    /// The per-run reset: a new `Start` seeds the live target with the
    /// configured value (the depth controller then adjusts it from there).
    #[test]
    fn start_seeds_effective_target_with_config() {
        let cfg = EngineConfig {
            transport: Transport::Udp,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 41_701,
            codec: CodecMode::Auto,
            target_ms: 12.0,
            capacity_ms: 100.0,
        };
        let handle = spawn_engine(Some(cfg));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(async {
            handle
                .cmd()
                .send(EngineCommand::Start(cfg))
                .await
                .expect("send Start");
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if handle.status().running {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let s = handle.status();
        assert!(s.running);
        assert!(
            (s.effective_target_ms - 12.0).abs() < 0.01,
            "fresh run targets the configured 12 ms, got {}",
            s.effective_target_ms
        );
        rt.block_on(async {
            handle
                .cmd()
                .send(EngineCommand::Stop)
                .await
                .expect("send Stop");
        });
    }
}
