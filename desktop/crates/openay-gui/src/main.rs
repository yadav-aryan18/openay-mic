//! OpenAY Mic desktop console — an Iced GUI over the `openay-server` engine,
//! with a best-effort StatusNotifier tray. Implements the "Studio rack at
//! night" design (`shared/design.md`): The Chain hero card, VU ladder, ON AIR
//! toggle, and a settings slide-over, all in the warm rack-unit palette.
//!
//! Window behaviour contract: the window's close button ALWAYS quits the
//! application cleanly (stop engine, unregister tray). There is no
//! hide-to-tray. The tray (if a StatusNotifierWatcher exists) offers
//! Show/Start/Stop/Quit; `--minimized` only hides the window at startup and
//! is ignored when no tray could be registered (the app must stay reachable).

mod app;
mod config;
mod icons;
mod theme;
mod tray;
mod vu;

use std::path::PathBuf;
use std::sync::Arc;

use iced::window;

use crate::app::{App, Flags};
use crate::tray::{spawn_tray, TrayBus};

/// The application's single window: 460x600, min 420x520, "OpenAY Mic".
fn window_settings(start_minimized: bool) -> window::Settings {
    window::Settings {
        size: iced::Size::new(460.0, 600.0),
        min_size: Some(iced::Size::new(420.0, 520.0)),
        visible: !start_minimized,
        resizable: true,
        decorations: true,
        // Close-requested is handled in `App::update`: it runs a cleanup
        // task (engine stop, tray unregister) and then `iced::exit()`s.
        // Keeping this false lets that cleanup happen before the runtime
        // terminates.
        exit_on_close_request: false,
        ..window::Settings::default()
    }
}

fn main() -> iced::Result {
    // CLI: `--minimized` starts tray-only (window hidden until Show).
    let cli_minimized = std::env::args().any(|a| a == "--minimized");

    // Load persisted settings (~/.config/openay-mic/config.toml).
    let config_path = config::config_path().unwrap_or_else(|_| PathBuf::from("config.toml"));
    let config = match config::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("openay-gui: failed to load config: {e:#}; using defaults");
            config::Config::default()
        }
    };

    // The engine: created once, reused across settings changes.
    let engine = openay_server::spawn_engine(None);

    // System tray (best-effort; the app works without one). If registration
    // fails or the desktop has no StatusNotifierWatcher, warn once and
    // continue — the app stays fully usable as a plain window, and the
    // window close button quits it.
    let tray_bus = Arc::new(TrayBus::default());
    let tray = match spawn_tray(tray_bus.clone()) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!(
                "openay-gui: warning: system tray unavailable ({e:#}); \
                 running window-only (the window close button quits the app)"
            );
            None
        }
    };

    // Only start hidden when there is a tray to restore the window; without
    // one the app would be unreachable.
    let minimized = (cli_minimized || config.start_minimized) && tray.is_some();

    let flags = Flags {
        engine,
        config,
        config_path,
        tray_bus,
        tray,
    };

    iced::application(App::title, App::update, App::view)
        .font(theme::CHAKRA_SEMIBOLD)
        .font(theme::CHAKRA_MEDIUM)
        .font(theme::PLEX_REGULAR)
        .font(theme::PLEX_MEDIUM)
        .default_font(theme::FONT_MONO)
        .window(window_settings(minimized))
        .theme(App::theme)
        .subscription(App::subscription)
        .run_with(move || App::new(flags))
}
