//! System tray icon via ksni (freedesktop StatusNotifierItem), on a
//! dedicated thread.
//!
//! The tray mirrors the engine state (idle / armed / live) with three
//! generated 24x24 pixmaps and exposes Show Console / Start / Stop / Quit.
//! Requests flow to the GUI through a shared [`TrayBus`] of atomics that the
//! application consumes on each tick (the tray has no access to the GUI's
//! window or config).

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use anyhow::Result;

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
pub struct TrayHandle {
    handle: ksni::Handle<AppIndicator>,
}

impl TrayHandle {
    /// Update the tray icon + tooltip state (idle/armed/live).
    pub fn set_state(&self, state: u8) {
        self.handle
            .update(|t| t.state.store(state, Ordering::Relaxed));
    }
}

/// Spawn the tray and return a handle. Start/Stop requests are routed
/// through the [`TrayBus`] and serviced by the GUI's tick (which owns the
/// config).
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

/// The ksni StatusNotifierItem implementation.
struct AppIndicator {
    state: Arc<AtomicU8>,
    bus: Arc<TrayBus>,
}

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
