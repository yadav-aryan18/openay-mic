//! System tray icon: ksni (freedesktop StatusNotifierItem) on Unix, on a
//! dedicated thread; window-only (no tray) on Windows.
//!
//! The tray mirrors the engine state (idle / armed / live) with three
//! generated 24x24 pixmaps and exposes Show Console / Start / Stop / Quit.
//! Requests flow to the GUI through a shared [`TrayBus`] of atomics that the
//! application consumes on each tick (the tray has no access to the GUI's
//! window or config).
//!
//! Platform split: [`TrayBus`] and the state constants are the
//! platform-neutral contract (pure atomics); only the ksni service is
//! Unix-specific. On Windows [`spawn_tray`] deliberately fails — `main`
//! already treats a missing tray as "window-only mode" (the window close
//! button quits, `--minimized` is ignored), which is the documented
//! behavior there. Windows tray support would need a Win32 tray-icon
//! integration into iced's event loop; not built yet.

#[cfg(unix)]
use anyhow::Result;
#[cfg(windows)]
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

#[cfg(unix)]
use crate::icons;

/// Tray states, in the same order as `icons::ICONS`.
pub const STATE_IDLE: u8 = 0;
pub const STATE_ARMED: u8 = 1;
pub const STATE_LIVE: u8 = 2;

/// One-way requests from the tray to the GUI; the GUI's tick consumes them.
#[derive(Default)]
pub struct TrayBus {
    show: AtomicBool,
    start: AtomicBool,
    stop: AtomicBool,
    quit: AtomicBool,
}

impl TrayBus {
    #[cfg(unix)]
    fn request(&self, which: &AtomicBool) {
        which.store(true, Ordering::Relaxed);
    }

    fn take(&self, which: &AtomicBool) -> bool {
        which.swap(false, Ordering::Relaxed)
    }

    /// The user clicked "Show Console": show and focus the window.
    pub fn take_show(&self) -> bool {
        self.take(&self.show)
    }

    /// The user clicked "Start".
    pub fn take_start(&self) -> bool {
        self.take(&self.start)
    }

    /// The user clicked "Stop".
    pub fn take_stop(&self) -> bool {
        self.take(&self.stop)
    }

    /// The user clicked "Quit": close the window and exit.
    pub fn take_quit(&self) -> bool {
        self.take(&self.quit)
    }
}

/// Owned handle to the running tray service.
#[cfg(unix)]
pub struct TrayHandle {
    handle: ksni::Handle<AppIndicator>,
}

#[cfg(unix)]
impl TrayHandle {
    /// Update the tray icon + tooltip state (idle/armed/live).
    pub fn set_state(&self, state: u8) {
        self.handle
            .update(|t| t.state.store(state, Ordering::Relaxed));
    }
}

/// Windows stub: the app runs window-only (no Win32 tray integration yet).
#[cfg(windows)]
pub struct TrayHandle;

#[cfg(windows)]
impl TrayHandle {
    /// No-op on Windows (no tray to update).
    pub fn set_state(&self, _state: u8) {}
}

/// Unix: spawn the ksni tray and return a handle. Start/Stop requests are
/// routed through the [`TrayBus`] and serviced by the GUI's tick (which owns
/// the config).
#[cfg(unix)]
pub fn spawn_tray(bus: Arc<TrayBus>) -> Result<TrayHandle> {
    let indicator = AppIndicator {
        state: Arc::new(AtomicU8::new(STATE_IDLE)),
        bus,
    };
    let service = ksni::TrayService::new(indicator);
    let handle = service.handle();
    service.spawn();
    Ok(TrayHandle { handle })
}

/// Windows: fail cleanly — main falls back to window-only mode with a
/// one-line warning, exactly like a Unix desktop without a
/// StatusNotifierWatcher.
#[cfg(windows)]
pub fn spawn_tray(_bus: Arc<TrayBus>) -> Result<TrayHandle> {
    Err(anyhow!(
        "system tray not supported on this platform yet (window-only mode)"
    ))
}

/// The ksni StatusNotifierItem implementation.
#[cfg(unix)]
struct AppIndicator {
    state: Arc<AtomicU8>,
    bus: Arc<TrayBus>,
}

#[cfg(unix)]
impl AppIndicator {
    fn show(&mut self) {
        self.bus.request(&self.bus.show);
    }

    fn start(&mut self) {
        self.bus.request(&self.bus.start);
    }

    fn stop(&mut self) {
        self.bus.request(&self.bus.stop);
    }

    fn quit(&mut self) {
        self.bus.request(&self.bus.quit);
    }

    /// Convert the generated RGBA pixel data to the freedesktop ARGB32
    /// network byte order expected by the StatusNotifierItem spec.
    fn pixmap_data(state: u8) -> Vec<u8> {
        let data = icons::ICONS
            .get(state as usize)
            .copied()
            .unwrap_or(&icons::IDLE);
        data.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[3], p[0], p[1], p[2]])
            .collect()
    }
}

#[cfg(unix)]
impl ksni::Tray for AppIndicator {
    fn id(&self) -> String {
        "openay-mic".to_string()
    }

    fn title(&self) -> String {
        "OpenAY Mic".to_string()
    }

    fn icon_name(&self) -> String {
        String::new() // we provide raw pixmaps, no themed icon
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let state = self.state.load(Ordering::Relaxed);
        vec![ksni::Icon {
            width: 24,
            height: 24,
            data: Self::pixmap_data(state),
        }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let subtitle = match self.state.load(Ordering::Relaxed) {
            STATE_IDLE => "idle",
            STATE_ARMED => "armed",
            _ => "live",
        };
        ksni::ToolTip {
            icon_name: String::new(),
            icon_pixmap: vec![],
            title: "OpenAY Mic".to_string(),
            description: format!("OpenAY Mic — {subtitle}"),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::MenuItem;
        let running = self.state.load(Ordering::Relaxed) != STATE_IDLE;
        vec![
            MenuItem::Standard(ksni::menu::StandardItem {
                label: "Show Console".to_string(),
                enabled: true,
                activate: Box::new(AppIndicator::show),
                ..Default::default()
            }),
            MenuItem::Separator,
            // Checkmark state mirrors the engine: Start is checked while
            // running, Stop is checked while stopped.
            MenuItem::Checkmark(ksni::menu::CheckmarkItem {
                label: "Start".to_string(),
                enabled: !running,
                checked: running,
                activate: Box::new(AppIndicator::start),
                ..Default::default()
            }),
            MenuItem::Checkmark(ksni::menu::CheckmarkItem {
                label: "Stop".to_string(),
                enabled: running,
                checked: !running,
                activate: Box::new(AppIndicator::stop),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".to_string(),
                enabled: true,
                activate: Box::new(AppIndicator::quit),
                ..Default::default()
            }),
        ]
    }
}
