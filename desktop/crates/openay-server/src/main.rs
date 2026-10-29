//! OpenAY Mic desktop receiver CLI: thin wrapper over the `openay_server`
//! engine library. Preserves all existing CLI flags, stats-line format, and
//! exit behavior.

use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use openay_server::{
    spawn_engine, CodecMode, ConfigError, EngineCommand, EngineConfig, Transport, MAX_PREBUFFER_MS,
    MIN_PREBUFFER_MS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
enum TransportArg {
    Udp,
    Tcp,
    /// Bluetooth SPP over BlueZ: registers the "OpenAY Mic SPP" profile on
    /// the default adapter and receives framed packets from paired phones
    /// over RFCOMM. `--bind`/`--port` are stored for display but unused.
    Bt,
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

#[derive(Parser, Debug, Clone)]
#[command(
    name = "openay-server",
    version,
    about = "OpenAY Mic desktop receiver: network -> jitter buffer -> PipeWire virtual microphone"
)]
struct Args {
    /// Transport to receive audio on: `udp`, `tcp`, or `bt` (Bluetooth SPP
    /// via BlueZ; needs a running bluetoothd and, without the `bluetooth`
    /// build feature, fails cleanly at startup).
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
    /// Output gain applied to decoded samples before the jitter buffer
    /// (1.0 = unity; clamped to 0..=4).
    #[arg(long, default_value_t = 1.0)]
    gain: f32,
    /// Play received audio on a local output device (ALSA on Linux, WASAPI
    /// on Windows) instead of the PipeWire virtual microphone. Requires the
    /// `audio-output` build feature.
    #[arg(long)]
    output: bool,
    /// LISTEN: also play the received audio on a local output device (mic
    /// testing) while the normal path keeps running. Uses --output-device
    /// when set. Requires the `audio-output` build feature.
    #[arg(long)]
    monitor: bool,
    /// With --output or --monitor: case-insensitive substring of the output
    /// device name to select (omit for the host default). Requires the
    /// `audio-output` build feature.
    #[arg(long)]
    output_device: Option<String>,
    /// List available output device names and exit. Requires the
    /// `audio-output` build feature.
    #[arg(long)]
    list_devices: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Audio-output requests in a build without the feature: clean error
    // before anything else runs (same pattern as a `bt` request without the
    // `bluetooth` feature).
    #[cfg(not(feature = "audio-output"))]
    if args.output || args.monitor || args.output_device.is_some() || args.list_devices {
        anyhow::bail!("built without audio output support (rebuild with --features audio-output)");
    }
    #[cfg(feature = "audio-output")]
    if args.output_device.is_some() && !args.output && !args.monitor {
        anyhow::bail!("--output-device requires --output or --monitor");
    }

    // `--list-devices`: print the host's output device names and exit 0.
    #[cfg(feature = "audio-output")]
    if args.list_devices {
        for name in openay_server::list_output_devices() {
            println!("{name}");
        }
        return Ok(());
    }

    let target_ms = args.target_ms.clamp(MIN_PREBUFFER_MS, MAX_PREBUFFER_MS);
    if (args.target_ms - target_ms).abs() > f32::EPSILON {
        eprintln!(
            "openay-server: --target-ms {} clamped to [{MIN_PREBUFFER_MS}, {MAX_PREBUFFER_MS}] => {target_ms}",
            args.target_ms
        );
    }

    let gain = args.gain.clamp(0.0, 4.0);
    if (args.gain - gain).abs() > f32::EPSILON {
        eprintln!(
            "openay-server: --gain {} clamped to [0, 4] => {gain}",
            args.gain
        );
    }

    #[cfg(not(feature = "pipewire"))]
    eprintln!("openay-server: built without PipeWire support — network+jitter only");

    let bind = resolve_bind(&args.bind, args.port).await?;
    let config = EngineConfig {
        audio_output: args.output,
        output_device: args.output_device.clone(),
        transport: match args.transport {
            TransportArg::Udp => Transport::Udp,
            TransportArg::Tcp => Transport::Tcp,
            TransportArg::Bt => Transport::Bluetooth,
        },
        bind,
        port: args.port,
        codec: match args.codec {
            CodecArg::Auto => CodecMode::Auto,
            CodecArg::Pcm => CodecMode::Pcm,
            CodecArg::Opus => CodecMode::Opus,
        },
        target_ms,
        capacity_ms: args.capacity_ms,
        gain,
        monitor: args.monitor,
    }
    .validated()
    .map_err(|e: ConfigError| anyhow::anyhow!(e))?;

    // The config is both the standby defaults for the handle and the first
    // Start payload (EngineConfig is never Copy — clone for the handle, move
    // into the Start payload below).
    let handle = spawn_engine(Some(config.clone()));

    // Cold-start contract: spawn_engine never binds or starts a pipeline —
    // the config it was given is just the defaults for the first Start. Send
    // Start right away so the CLI binds immediately after launch (scripts
    // and smoke flows depend on the port being live within ~200 ms).
    handle.cmd().send(EngineCommand::Start(config)).await?;

    // Wait for Ctrl-C, or for an early engine stop (bind failure / PipeWire
    // setup failure). Whichever fires first ends the wait.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let s = handle.status();
                if !s.running && handle.last_error().is_some() {
                    break;
                }
            }
        }
    }

    handle.cmd().send(EngineCommand::Stop).await?;

    // Wait for the engine to finish stopping (the network task exits within
    // ~200 ms; the PipeWire thread may need up to 3 s).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while handle.status().running {
        if std::time::Instant::now() >= deadline {
            eprintln!("openay-server: engine did not stop in time");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    if let Some(line) = handle.take_stats_line() {
        println!("{line}");
    }

    if let Some(msg) = handle.last_error() {
        anyhow::bail!("{msg}");
    }
    Ok(())
}

/// Resolve a bind address string to an `IpAddr`.  Tries `IpAddr::from_str`
/// first; if that fails (hostname), resolves via `tokio::net::lookup_host`
/// and takes the first address.
async fn resolve_bind(bind: &str, port: u16) -> Result<IpAddr> {
    if let Ok(ip) = IpAddr::from_str(bind) {
        return Ok(ip);
    }
    let addrs = tokio::net::lookup_host((bind, port))
        .await
        .with_context(|| format!("resolving bind address `{bind}`"))?;
    addrs
        .into_iter()
        .next()
        .map(|sa| sa.ip())
        .context("no addresses resolved for bind")
}
