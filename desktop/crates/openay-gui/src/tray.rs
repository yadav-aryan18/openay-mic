//! System tray icon: ksni (freedesktop StatusNotifierItem) on Unix,
//! tray-icon's Win32 Shell_NotifyIcon on Windows.
//!
//! The tray mirrors the engine state (idle / armed / live) with three
//! generated 24x24 pixmaps and exposes Show Console / Start / Stop / Quit.
//! Requests flow to the GUI through a shared [`TrayBus`] of atomics that the
//! application consumes on each tick (the tray has no access to the GUI's
//! window or config).
//!
//! Platform split: [`TrayBus`] and the state constants are the
//! platform-neutral contract (pure atomics). The service behind
//! [`spawn_tray`] is per-platform — ksni (freedesktop StatusNotifierItem) on
//! Unix, tray-icon's Win32 Shell_NotifyIcon on Windows — with an identical
//! menu (Show Console / Start / Stop / Quit) and the same generated 24x24
//! pixmaps.
//!
//! Windows threading contract (tray-icon 0.26 docs): `TrayIcon` is !Send +
//! !Sync and the thread that created it must run an event loop. iced runs
//! `App::new` and `update` on the main thread *inside* the winit event loop,
//! so the tray is created in `App::new` (never in `main`, which precedes the
//! loop) and mutated only from `update`; menu/click events are drained from
//! the crate's global crossbeam receivers by a background subscription and
//! translated into [`TrayBus`] requests that the GUI's tick consumes — the
//! exact flow the ksni impl uses.

#[cfg(unix)]
use anyhow::Result;
#[cfg(windows)]
use anyhow::{anyhow, Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

#[cfg(unix)]
use crate::icons;
#[cfg(windows)]
use crate::icons::{ARMED, IDLE, LIVE};

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

// Main-thread storage for the Win32 tray icon: `TrayIcon` is !Send, and
// every toucher (`spawn_tray` from `App::new`, `set_state` from `update`)
// runs on the main thread inside iced's event loop. The icon lives until
// process exit (the window-close contract is quit-the-process anyway).
#[cfg(windows)]
thread_local! {
    static TRAY_ICON: std::cell::RefCell<Option<tray_icon::TrayIcon>> =
        const { std::cell::RefCell::new(None) };
}

/// Windows handle: a `Send` token carrying the menu ids for the event-pump
/// subscription. The tray icon itself never leaves the main thread (see
/// [`TRAY_ICON`]).
#[cfg(windows)]
#[derive(Clone)]
pub struct TrayHandle {
    menu_ids: WindowsMenuIds,
}

#[cfg(windows)]
impl TrayHandle {
    /// Update the tray icon + tooltip state (idle/armed/live). Main thread.
    pub fn set_state(&self, state: u8) {
        let (pixmap, label) = match state {
            STATE_LIVE => (&LIVE, "live"),
            STATE_ARMED => (&ARMED, "armed"),
            _ => (&IDLE, "idle"),
        };
        let icon = match tray_icon::Icon::from_rgba(pixmap.to_vec(), 24, 24) {
            Ok(icon) => icon,
            Err(e) => {
                eprintln!("openay-gui: tray icon decode failed: {e}");
                return;
            }
        };
        TRAY_ICON.with(|slot| {
            // The borrow lasts only for the icon/tooltip swap — no re-entrancy.
            let slot = slot.borrow();
            let Some(tray) = slot.as_ref() else {
                return;
            };
            if let Err(e) = tray
                .set_icon(Some(icon))
                .and_then(|_| tray.set_tooltip(Some(format!("OpenAY Mic — {label}"))))
            {
                eprintln!("openay-gui: tray state update failed: {e}");
            }
        });
    }
}

/// Windows: create the Shell_NotifyIcon tray on the calling thread (the
/// main thread, with iced/winit's event loop already running — the crate
/// requires an event loop on the creating thread).
#[cfg(windows)]
pub fn spawn_tray(
    _bus: Arc<TrayBus>, // events are drained via the global receivers, not the bus
) -> Result<TrayHandle> {
    use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};

    // muda's Menu uses interior mutability: `append` takes &self.
    let menu = Menu::new();
    let ids = WindowsMenuIds {
        show: MenuId::new("show"),
        start: MenuId::new("start"),
        stop: MenuId::new("stop"),
        quit: MenuId::new("quit"),
    };
    menu.append(&MenuItem::with_id(
        ids.show.clone(),
        "Show Console",
        true,
        None::<tray_icon::menu::accelerator::Accelerator>,
    ))
    .context("tray menu: Show item")?;
    menu.append(&MenuItem::with_id(
        ids.start.clone(),
        "Start",
        true,
        None::<tray_icon::menu::accelerator::Accelerator>,
    ))
    .context("tray menu: Start item")?;
    menu.append(&MenuItem::with_id(
        ids.stop.clone(),
        "Stop",
        true,
        None::<tray_icon::menu::accelerator::Accelerator>,
    ))
    .context("tray menu: Stop item")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("tray menu: separator")?;
    menu.append(&MenuItem::with_id(
        ids.quit.clone(),
        "Quit",
        true,
        None::<tray_icon::menu::accelerator::Accelerator>,
    ))
    .context("tray menu: Quit item")?;

    let icon = tray_icon::Icon::from_rgba(IDLE.to_vec(), 24, 24)
        .context("tray icon: invalid pixmap")?;
    let tray = tray_icon::TrayIconBuilder::new()
        .with_id("openay-mic")
        .with_icon(icon)
        .with_tooltip("OpenAY Mic — idle")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .build()
        .map_err(|e| anyhow!("tray registration failed: {e}"))?;
    TRAY_ICON.with(|slot| *slot.borrow_mut() = Some(tray));
    Ok(TrayHandle { menu_ids: ids })
}

/// The menu item ids, kept on the handle so the event-polling subscription
/// can match events without stringly-typed reconstruction.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct WindowsMenuIds {
    pub show: tray_icon::menu::MenuId,
    pub start: tray_icon::menu::MenuId,
    pub stop: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

/// Drain the crate's global menu/click receivers (called from the GUI's
/// background subscription): translate menu actions into [`TrayBus`]
/// requests the GUI tick consumes, and drain click events (left click opens
/// the menu; nothing to act on).
#[cfg(windows)]
pub fn poll_events(handle: &TrayHandle, bus: &TrayBus) {
    use tray_icon::menu::MenuEvent;

    while let Ok(event) = MenuEvent::receiver().try_recv() {
        let id = event.id();
        if id == &handle.menu_ids.show {
            bus.request(&bus.show);
        } else if id == &handle.menu_ids.start {
            bus.request(&bus.start);
        } else if id == &handle.menu_ids.stop {
            bus.request(&bus.stop);
        } else if id == &handle.menu_ids.quit {
            bus.request(&bus.quit);
        }
    }
    while let Ok(_click) = tray_icon::TrayIconEvent::receiver().try_recv() {
        // Clicks open the menu (with_menu_on_left_click); nothing to do.
    }
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
