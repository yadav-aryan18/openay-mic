//! User settings persistence: `~/.config/openay-mic/config.toml` and the XDG
//! autostart entry `~/.config/autostart/openay-mic.desktop`.
//!
//! The config is the GUI's own layer on top of [`openay_server::EngineConfig`];
//! everything here is pure (paths are parameters) so it is fully
//! unit-testable without touching the real home directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use openay_server::{CodecMode, EngineConfig, Transport};

/// Config file name inside the config dir.
pub const CONFIG_FILE: &str = "config.toml";
/// Autostart entry file name (in `~/.config/autostart/`).
pub const AUTOSTART_FILE: &str = "openay-mic.desktop";
/// Directory the app keeps its config in (relative to the XDG config dir).
pub const CONFIG_DIR: &str = "openay-mic";
/// Default listen port (protocol spec).
pub const DEFAULT_PORT: u16 = 41_700;
/// Default bind address: all interfaces.
pub const DEFAULT_BIND: &str = "0.0.0.0";
/// Default jitter target in ms (design.md chain: `10.0 ms`).
pub const DEFAULT_TARGET_MS: f32 = 10.0;

/// The persisted user settings. Serialized as TOML; every field has a
/// default so a missing or partial file loads fine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub bind: String,
    pub codec: String,
    pub target_ms: f32,
    pub autostart: bool,
    pub start_minimized: bool,
    /// Drop cable pulses and the power-on stagger (accessibility).
    pub reduce_motion: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: DEFAULT_PORT,
            bind: DEFAULT_BIND.to_string(),
            codec: "auto".to_string(),
            target_ms: DEFAULT_TARGET_MS,
            autostart: false,
            start_minimized: false,
            reduce_motion: false,
        }
    }
}

/// The config codec as the engine enum (unknown strings fall back to Auto).
impl Config {
    pub fn codec_mode(&self) -> CodecMode {
        match self.codec.to_ascii_lowercase().as_str() {
            "pcm" => CodecMode::Pcm,
            "opus" => CodecMode::Opus,
            _ => CodecMode::Auto,
        }
    }

    /// The config as an engine config (bind parsed; unparseable -> 0.0.0.0).
    pub fn to_engine(&self) -> EngineConfig {
        let bind = self
            .bind
            .parse()
            .unwrap_or_else(|_| "0.0.0.0".parse().expect("static IP"));
        EngineConfig {
            transport: Transport::Udp,
            bind,
            port: self.port,
            codec: self.codec_mode(),
            target_ms: self.target_ms,
            capacity_ms: 100.0,
        }
    }
}

/// The config directory: `$XDG_CONFIG_HOME/openay-mic` (or
/// `~/.config/openay-mic`).
pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("no XDG config home")?;
    Ok(base.join(CONFIG_DIR))
}

/// The autostart directory: `$XDG_CONFIG_HOME/autostart`.
pub fn autostart_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("no XDG config home")?;
    Ok(base.join("autostart"))
}

/// The config file path.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE))
}

/// The autostart entry path.
pub fn autostart_path() -> Result<PathBuf> {
    Ok(autostart_dir()?.join(AUTOSTART_FILE))
}

/// Load the config from `path`. A missing file (or unreadable) yields
/// [`Config::default`]; a parse failure is an error.
pub fn load_config(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).context("parsing config.toml"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).context("reading config.toml"),
    }
}

/// Serialize the config as TOML text.
pub fn config_to_toml(config: &Config) -> Result<String> {
    toml::to_string_pretty(config).context("serializing config")
}

/// Save the config to `path`, creating parent directories.
pub fn save_config(config: &Config, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating config dir {}", dir.display()))?;
    }
    let text = config_to_toml(config)?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

// ---------------------------------------------------------------------------
// XDG autostart entry
// ---------------------------------------------------------------------------

/// The `.desktop` autostart entry for this binary (`$argv0`).
pub fn autostart_entry(exec_path: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=OpenAY Mic\n\
         Comment=OpenAY Mic desktop console\n\
         Exec={exec_path} --minimized\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n\
         Terminal=false\n"
    )
}

/// A parsed `.desktop` entry (the fields we care about).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartEntry {
    pub exec: String,
    pub name: String,
    pub enabled: bool,
}

/// Parse a `.desktop` entry; returns `None` if it is not an `Application`
/// entry or lacks an `Exec` line.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_autostart(contents: &str) -> Option<AutostartEntry> {
    let mut exec = None;
    let mut name = None;
    let mut enabled = None;
    let mut is_application = false;
    for line in contents.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Type=") {
            is_application = v.eq_ignore_ascii_case("Application");
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("X-GNOME-Autostart-enabled=") {
            enabled = Some(v.eq_ignore_ascii_case("true"));
        }
    }
    let exec = exec?;
    if !is_application {
        return None;
    }
    Some(AutostartEntry {
        exec,
        name: name.unwrap_or_default(),
        enabled: enabled.unwrap_or(true),
    })
}

/// Write the autostart entry to `path` (creating the directory). Returns the
/// path written.
pub fn write_autostart(path: &Path, exec_path: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating autostart dir {}", dir.display()))?;
    }
    fs::write(path, autostart_entry(exec_path))
        .with_context(|| format!("writing {}", path.display()))
}

/// Remove the autostart entry; `Ok(false)` if it did not exist.
pub fn remove_autostart(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Apply the `autostart` preference: writes or removes the entry using
/// `exec_path` as the Exec value. Returns whether the file now exists.
pub fn apply_autostart(enabled: bool, path: &Path, exec_path: &str) -> Result<bool> {
    if enabled {
        write_autostart(path, exec_path)?;
        Ok(true)
    } else {
        remove_autostart(path)?;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn tempdir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("openay-mic-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn defaults_on_missing_file() {
        let dir = tempdir("missing");
        let path = dir.join("config.toml");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg, Config::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_on_empty_file() {
        let dir = tempdir("empty");
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "").unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg, Config::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_on_partial_file() {
        let dir = tempdir("partial");
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "port = 43210\n").unwrap();
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.port, 43210);
        assert_eq!(cfg.bind, DEFAULT_BIND, "missing fields keep defaults");
        assert_eq!(cfg.codec, "auto");
        assert_eq!(cfg.target_ms, DEFAULT_TARGET_MS);
        assert!(!cfg.autostart);
        assert!(!cfg.start_minimized);
        assert!(!cfg.reduce_motion);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn toml_roundtrip() {
        let cfg = Config {
            port: 41_700,
            bind: "127.0.0.1".to_string(),
            codec: "opus".to_string(),
            target_ms: 15.0,
            autostart: true,
            start_minimized: true,
            reduce_motion: true,
        };
        let text = config_to_toml(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg, "serialize -> parse must roundtrip");
    }

    #[test]
    fn save_then_load() {
        let dir = tempdir("save");
        let path = dir.join("config.toml");
        let cfg = Config {
            port: 42_000,
            bind: "0.0.0.0".to_string(),
            codec: "pcm".to_string(),
            target_ms: 5.0,
            autostart: false,
            start_minimized: true,
            reduce_motion: false,
        };
        save_config(&cfg, &path).unwrap();
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded, cfg);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codec_mode_mapping() {
        let cfg = Config {
            codec: "PCM".to_string(),
            ..Config::default()
        };
        assert_eq!(cfg.codec_mode(), CodecMode::Pcm);
        let cfg = Config {
            codec: "opus".to_string(),
            ..Config::default()
        };
        assert_eq!(cfg.codec_mode(), CodecMode::Opus);
        let cfg = Config {
            codec: "weird".to_string(),
            ..Config::default()
        };
        assert_eq!(cfg.codec_mode(), CodecMode::Auto);
        let cfg = Config {
            codec: "auto".to_string(),
            ..Config::default()
        };
        assert_eq!(cfg.codec_mode(), CodecMode::Auto);
    }

    #[test]
    fn engine_config_translation() {
        let cfg = Config {
            bind: "127.0.0.1".to_string(),
            port: 12_345,
            codec: "pcm".to_string(),
            target_ms: 17.0,
            ..Config::default()
        };
        let e = cfg.to_engine();
        assert_eq!(e.bind, "127.0.0.1".parse::<std::net::IpAddr>().unwrap());
        assert_eq!(e.port, 12_345);
        assert_eq!(e.codec, CodecMode::Pcm);
        assert_eq!(e.target_ms, 17.0);
        assert_eq!(e.transport, Transport::Udp);
        // Unparseable bind falls back to 0.0.0.0.
        let cfg = Config {
            bind: "not-an-ip".to_string(),
            ..Config::default()
        };
        assert_eq!(
            cfg.to_engine().bind,
            "0.0.0.0".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn autostart_entry_content() {
        let text = autostart_entry("/usr/bin/openay-gui");
        assert!(text.contains("[Desktop Entry]"));
        assert!(text.contains("Type=Application"));
        assert!(text.contains("Exec=/usr/bin/openay-gui --minimized"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true"));
        assert!(text.contains("Name=OpenAY Mic"));
    }

    #[test]
    fn autostart_parse_roundtrip() {
        let text = autostart_entry("/opt/openay/openay-gui");
        let parsed = parse_autostart(&text).expect("parseable");
        assert_eq!(parsed.exec, "/opt/openay/openay-gui --minimized");
        assert_eq!(parsed.name, "OpenAY Mic");
        assert!(parsed.enabled);
    }

    #[test]
    fn autostart_parse_rejects_non_application() {
        let text = "[Desktop Entry]\nType=Link\nExec=/bin/true\n";
        assert!(
            parse_autostart(text).is_none(),
            "Link entries are not autostart apps"
        );
    }

    #[test]
    fn autostart_parse_requires_exec() {
        let text = "[Desktop Entry]\nType=Application\nName=OpenAY Mic\n";
        assert!(parse_autostart(text).is_none());
    }

    #[test]
    fn autostart_write_and_remove() {
        let dir = tempdir("autostart");
        let path = dir.join("openay-mic.desktop");
        assert!(!remove_autostart(&path).unwrap(), "no file yet");

        apply_autostart(true, &path, "/usr/bin/openay-gui").unwrap();
        assert!(path.exists());
        let parsed = parse_autostart(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.exec.starts_with("/usr/bin/openay-gui"));

        apply_autostart(false, &path, "/usr/bin/openay-gui").unwrap();
        assert!(!path.exists(), "entry removed");
        let _ = fs::remove_dir_all(&dir);
    }
}
