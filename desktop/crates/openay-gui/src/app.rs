//! The Iced application: state, messages, update, and view.
//! Canvas programs for the MIC level ring, VU ladder, cables, and lamp dots
//! are defined inline.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::alignment;
use iced::mouse::Cursor;
use iced::widget::canvas::{self, Frame, Geometry, Path, Program, Stroke};
use iced::widget::{
    button, column, container, pick_list, row, scrollable, slider, text, text_input, toggler,
    Space, Stack,
};
use iced::{
    window, Color, Element, Length, Point, Radians, Rectangle, Size, Subscription, Task, Theme,
};

use openay_server::{CodecMode, EngineCommand, EngineConfig, EngineHandle, EngineStatus};

use crate::config::{self, apply_autostart, autostart_path, Config};
use crate::theme;
use crate::tray::{TrayBus, TrayHandle, STATE_ARMED, STATE_IDLE, STATE_LIVE};
use crate::vu::{self, VuBallistics};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Engine status polling interval.
const TICK_MS: u64 = 200;
/// Power-on stagger: stages light left-to-right over ~400 ms.
const STAGGER_DURATION: f32 = 0.40;
/// Cable pulse frequency (Hz).
const CABLE_PULSE_HZ: f32 = 1.0;
/// Level ring smoothing time constant (seconds).
const RING_TAU: f32 = 0.10;
/// ON AIR press pulse duration (seconds; design.md: 150-250 ms).
const PRESS_PULSE_MS: f32 = 0.20;

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Flags passed from the main entry point into the app.
pub struct Flags {
    pub engine: EngineHandle,
    pub config: Config,
    pub config_path: PathBuf,
    pub tray_bus: Arc<TrayBus>,
    pub tray: Option<TrayHandle>,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    // Engine
    engine: EngineHandle,
    engine_cfg: EngineConfig, // last config sent to the engine
    status: EngineStatus,
    // VU / display
    vu: VuBallistics,
    ring_level: f32,
    last_received: u64,
    pps: f32,
    streaming: bool,
    last_tick: Instant,
    // Window
    window_id: Option<window::Id>,
    // Settings panel
    settings_open: bool,
    port_input: String,
    bind_input: String,
    bind_options: Vec<String>,
    codec: CodecMode,
    target_ms: f32,
    autostart: bool,
    start_minimized: bool,
    reduce_motion: bool,
    dirty: bool,
    // Persistence
    config: Config,
    config_path: PathBuf,
    tray_bus: Arc<TrayBus>,
    tray: Option<TrayHandle>,
    // Stagger
    stagger: bool,
    stagger_t: f32,
    // ON AIR press pulse (visual only; cleared on tick)
    air_pulse: Option<Instant>,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Message {
    Tick(Instant),
    ToggleAir,
    ToggleSettings,
    PortChanged(String),
    BindSelected(String),
    CodecSelected(CodecMode),
    TargetChanged(f32),
    AutostartToggled(bool),
    MinimizedToggled(bool),
    MotionToggled(bool),
    SaveSettings,
    CloseRequested(window::Id),
    WindowId(Option<window::Id>),
    CmdSent,
    /// Final step of a quit: engine stopped and tray dropped, exit the runtime.
    Exit,
}

// ---------------------------------------------------------------------------
// App impl
// ---------------------------------------------------------------------------

impl App {
    pub fn new(flags: Flags) -> (Self, Task<Message>) {
        let config = flags.config.clone();
        let engine_cfg = config.to_engine();
        let port_input = config.port.to_string();
        let bind_input = config.bind.clone();
        let codec = config.codec_mode();
        let target_ms = config.target_ms;
        let autostart = config.autostart;
        let start_minimized = config.start_minimized;
        let reduce_motion = config.reduce_motion;
        let bind_options = get_local_ips(&bind_input);
        let initial_status = flags.engine.status();

        let app = App {
            engine: flags.engine,
            engine_cfg,
            status: initial_status,
            vu: VuBallistics::default(),
            ring_level: 0.0,
            last_received: 0,
            pps: 0.0,
            streaming: false,
            last_tick: Instant::now(),
            window_id: None,
            settings_open: false,
            port_input,
            bind_input,
            bind_options,
            codec,
            target_ms,
            autostart,
            start_minimized,
            reduce_motion,
            dirty: false,
            config,
            config_path: flags.config_path,
            tray_bus: flags.tray_bus,
            tray: flags.tray,
            stagger: false,
            stagger_t: 0.0,
            air_pulse: None,
        };

        // Request the window ID (needed for show/hide from the tray).
        let task = window::get_oldest().map(Message::WindowId);
        (app, task)
    }

    pub fn title(&self) -> String {
        "OpenAY Mic".to_string()
    }

    pub fn theme(&self) -> Theme {
        Theme::custom(
            "OpenAY".into(),
            iced::theme::Palette {
                background: theme::INK,
                text: theme::CREAM,
                primary: theme::AMBER,
                // success is unused by any widget in this app; set to a token
                // color to avoid ad-hoc hex values outside the palette.
                success: theme::DIM,
                danger: theme::TALLY,
            },
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick(now) => self.tick(now),
            Message::ToggleAir => self.toggle_air(),
            Message::ToggleSettings => {
                self.settings_open = !self.settings_open;
                Task::none()
            }
            Message::PortChanged(s) => {
                self.port_input = s;
                self.dirty = true;
                Task::none()
            }
            Message::BindSelected(s) => {
                self.bind_input = s.clone();
                self.dirty = true;
                Task::none()
            }
            Message::CodecSelected(c) => {
                self.codec = c;
                self.dirty = true;
                Task::none()
            }
            Message::TargetChanged(v) => {
                self.target_ms = v;
                self.dirty = true;
                Task::none()
            }
            Message::AutostartToggled(b) => {
                self.autostart = b;
                self.dirty = true;
                Task::none()
            }
            Message::MinimizedToggled(b) => {
                self.start_minimized = b;
                self.dirty = true;
                Task::none()
            }
            Message::MotionToggled(b) => {
                self.reduce_motion = b;
                self.dirty = true;
                Task::none()
            }
            Message::SaveSettings => self.save_settings(),
            Message::CloseRequested(_id) => {
                // The window X (or Alt+F4) quits the application cleanly:
                // stop the engine, unregister the tray, then exit. The old
                // hide-to-tray behavior is gone — on stock GNOME there is no
                // StatusNotifier tray to bring the window back, so hiding
                // would make the app unreachable.
                self.quit()
            }
            Message::WindowId(id) => {
                self.window_id = id;
                Task::none()
            }
            Message::CmdSent => Task::none(),
            Message::Exit => iced::exit(),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(vec![
            iced::time::every(Duration::from_millis(TICK_MS)).map(Message::Tick),
            window::close_requests().map(Message::CloseRequested),
        ])
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn tick(&mut self, now: Instant) -> Task<Message> {
        let dt = (now - self.last_tick).as_secs_f32().clamp(0.0, 0.5);
        self.last_tick = now;

        // Poll engine status (consumes the level-peak interval).
        let status = self.engine.status();
        let received_delta = status.received.saturating_sub(self.last_received);
        self.last_received = status.received;
        self.pps = vu::pps(received_delta, dt);
        self.streaming = status.running && received_delta > 0;
        self.status = status;

        // VU ballistics: instant attack, ~12 dB/s decay.
        self.vu.update(status.level_peak, dt);

        // MIC ring smoothing (~100 ms time constant).
        let alpha = 1.0 - (-dt / RING_TAU).exp();
        self.ring_level += (status.level_peak - self.ring_level) * alpha;

        // Stagger animation (skipped under reduced motion).
        if self.stagger && self.status.running {
            self.stagger_t += dt;
            if self.stagger_t >= STAGGER_DURATION {
                self.stagger = false;
            }
        }

        // ON AIR press pulse: expire after PRESS_PULSE_MS (rendered by the
        // toggle's halo canvas; ticks give it a short two-frame flash).
        if self.air_pulse.is_some_and(|t0| t0.elapsed().as_secs_f32() >= PRESS_PULSE_MS) {
            self.air_pulse = None;
        }

        // Tray requests.
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if self.tray_bus.take_quit() {
            tasks.push(self.quit());
        }
        if self.tray_bus.take_show() {
            if let Some(id) = self.window_id {
                tasks.push(window::change_mode(id, window::Mode::Windowed));
                tasks.push(window::gain_focus(id));
            }
        }
        if self.tray_bus.take_start() {
            tasks.push(self.engine_cmd(EngineCommand::Start(self.engine_cfg)));
        }
        if self.tray_bus.take_stop() {
            tasks.push(self.engine_cmd(EngineCommand::Stop));
        }

        // Tray icon state: idle / armed / live.
        let tray_state = if !self.status.running {
            STATE_IDLE
        } else if self.streaming {
            STATE_LIVE
        } else {
            STATE_ARMED
        };
        if let Some(tray) = &self.tray {
            tray.set_state(tray_state);
        }

        Task::batch(tasks)
    }

    fn toggle_air(&mut self) -> Task<Message> {
        self.air_pulse = Some(Instant::now());
        if self.status.running {
            self.stagger = false;
            self.engine_cmd(EngineCommand::Stop)
        } else {
            if !self.reduce_motion {
                self.stagger = true;
                self.stagger_t = 0.0;
            }
            self.engine_cmd(EngineCommand::Start(self.engine_cfg))
        }
    }

    /// Quit the application cleanly: unregister the tray (dropping the ksni
    /// handle removes the StatusNotifierItem), send the engine a stop, and
    /// only then exit the iced runtime.
    fn quit(&mut self) -> Task<Message> {
        self.tray = None;
        let sender = self.engine.cmd();
        Task::perform(
            async move {
                let _ = sender.send(EngineCommand::Stop).await;
            },
            |_| Message::Exit,
        )
    }

    fn engine_cmd(&self, cmd: EngineCommand) -> Task<Message> {
        engine_cmd(&self.engine, cmd)
    }

    fn save_settings(&mut self) -> Task<Message> {
        // Validate the port (numeric, 1..=65535).
        let Ok(port) = self.port_input.parse::<u16>() else {
            return Task::none();
        };
        if port == 0 {
            return Task::none();
        }

        let cfg = Config {
            port,
            bind: self.bind_input.clone(),
            codec: self.codec.as_str().to_string(),
            target_ms: self.target_ms.clamp(
                openay_server::MIN_PREBUFFER_MS,
                openay_server::MAX_PREBUFFER_MS,
            ),
            autostart: self.autostart,
            start_minimized: self.start_minimized,
            reduce_motion: self.reduce_motion,
        };
        let engine_cfg = cfg.to_engine();

        let path = self.config_path.clone();
        let autostart_path = autostart_path().unwrap_or_default();
        let autostart_on = cfg.autostart;
        let exec_path = std::env::current_exe()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let sender = self.engine.cmd();
        let was_running = self.status.running;

        self.config = cfg.clone();
        self.engine_cfg = engine_cfg;
        self.dirty = false;

        // Persist + (if running) restart the engine with the new config.
        Task::perform(
            async move {
                let _ = config::save_config(&cfg, &path);
                let _ = apply_autostart(autostart_on, &autostart_path, &exec_path);
                if was_running {
                    let _ = sender.send(EngineCommand::Stop).await;
                    let _ = sender.send(EngineCommand::Start(engine_cfg)).await;
                }
            },
            |_| Message::CmdSent,
        )
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

impl App {
    pub fn view<'a>(&'a self) -> Element<'a, Message> {
        // Single vertical flow per design.md: header / Chain hero panel /
        // VU ladder row / centered ON AIR toggle / status line. The root
        // column is explicitly Fill x Fill and centers its children
        // horizontally (the 100 px toggle must sit centered, not left).
        let content = column![
            self.header(),
            self.hero_chain(),
            self.vu_ladder(),
            self.air_toggle(),
            Space::new(Length::Fill, Length::Fill),
            self.status_line(),
        ]
        .spacing(12)
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(alignment::Horizontal::Center);

        if self.settings_open {
            Stack::with_children(vec![
                content.into(),
                // Scrim: click to close.
                button(Space::new(Length::Fill, Length::Fill))
                    .on_press(Message::ToggleSettings)
                    .style(|_t, _s| button::Style {
                        background: Some(iced::Background::Color(Color::from_rgba(
                            0.0, 0.0, 0.0, 0.45,
                        ))),
                        ..button::Style::default()
                    })
                    .into(),
                // Settings panel pinned to the right edge. The row MUST be
                // given an explicit Fill height: with the default Shrink
                // height, flex lays out Fill-height children against a zero
                // cross size and the panel collapses to nothing.
                row![
                    Space::with_width(Length::Fill),
                    self.settings_panel()
                ]
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            ])
            .into()
        } else {
            content.into()
        }
    }

    // ---- Header: lamp dot + wordmark + menu button ----

    fn header<'a>(&'a self) -> Element<'a, Message> {
        // Lamp dot: amber while the link is up, line when idle. NOT tally —
        // design.md binds tally red to exactly two places (the ON AIR lamp
        // and the VU clip zone), so the header lamp reads the "hot" amber
        // like the rest of the chain.
        let lamp_color = if self.status.running {
            theme::AMBER
        } else {
            theme::LINE
        };
        let lamp = canvas::Canvas::new(LampDot { color: lamp_color })
            .width(Length::Fixed(14.0))
            .height(Length::Fixed(14.0));

        let menu_dot: Element<Message> = if self.dirty {
            canvas::Canvas::new(DirtyDot)
                .width(Length::Fixed(6.0))
                .height(Length::Fixed(6.0))
                .into()
        } else {
            Space::new(6.0, 6.0).into()
        };

        let menu_btn = button(
            row![
                // Canvas-drawn hamburger: the "≡" glyph (U+2261) does not
                // exist in Chakra Petch and rendered as a tofu box.
                canvas::Canvas::new(MenuIcon)
                    .width(Length::Fixed(16.0))
                    .height(Length::Fixed(16.0)),
                menu_dot,
            ]
            .spacing(4)
            .align_y(alignment::Vertical::Center),
        )
        .on_press(Message::ToggleSettings)
        // Symmetric 8 px padding -> >= 32x32 px hit area (design.md header
        // contract); the default button padding is 5/10 and off-center.
        .padding(8)
        .style(theme::flat_button);

        row![
            lamp,
            Space::new(8.0, 8.0),
            text("OPENAY MIC")
                .font(theme::FONT_HEADER)
                .size(16)
                .color(theme::CREAM),
            // Width-only spacer: `Space(Fill, Fill)` would make this row
            // Fill-HEIGHT too (Row::push encloses child size hints), turning
            // the header into a flex-fill child that swallows half the
            // leftover vertical space of the root column.
            Space::with_width(Length::Fill),
            menu_btn,
        ]
        .width(Length::Fill)
        .align_y(alignment::Vertical::Center)
        .into()
    }

    // ---- The Chain (hero card) ----

    /// Height of the stage-card body region; keeps the three cards equal
    /// sized (the MIC ring is 76 px, so the LINK/CONSOLE bodies center
    /// within the same footprint).
    const STAGE_BODY_HEIGHT: f32 = 76.0;

    fn hero_chain<'a>(&'a self) -> Element<'a, Message> {
        let hot = self.status.running && self.streaming;

        // Value color per stage: LINE during the power-on stagger, DIM while
        // the strip is cold, CREAM once hot.
        let lit = |i: usize| -> bool {
            !self.stagger || self.stagger_t >= (i as f32) * (STAGGER_DURATION / 3.0)
        };
        let value_color = |lit: bool| -> Color {
            if !lit {
                theme::LINE
            } else if hot {
                theme::CREAM
            } else {
                theme::DIM
            }
        };

        let mic_card = self.stage_card(
            "MIC",
            canvas::Canvas::new(LevelRing {
                level: self.ring_level,
                hot,
                lit: lit(0),
            })
            .width(Length::Fixed(76.0))
            .height(Length::Fixed(76.0)),
        );

        let link_card = self.stage_card(
            "LINK",
            column![
                text(format!("{:.0}/s", self.pps))
                    .font(theme::FONT_MONO_MEDIUM)
                    .size(20)
                    .color(value_color(lit(1))),
                text(format!("{} LOST", self.status.lost))
                    .font(theme::FONT_MONO)
                    .size(12)
                    // NOT tally: design.md binds tally red to exactly two
                    // places (ON AIR lamp, VU clip zone); loss is reported
                    // by the number itself.
                    .color(value_color(lit(1))),
            ]
            .spacing(4)
            .align_x(alignment::Horizontal::Center),
        );

        let console_card = self.stage_card(
            "CONSOLE",
            column![
                text(format!("{:.1} ms", self.engine_cfg.target_ms))
                    .font(theme::FONT_MONO_MEDIUM)
                    .size(20)
                    .color(value_color(lit(2))),
                canvas::Canvas::new(FillBar {
                    fraction: (self.status.fill_ms / 100.0).clamp(0.0, 1.0),
                    hot,
                })
                .width(Length::Fixed(64.0))
                .height(Length::Fixed(6.0)),
            ]
            .spacing(4)
            .align_x(alignment::Horizontal::Center),
        );

        // Cable color: pulses ~1 Hz while audio flows; cold when idle.
        // During the power-on stagger the cables pulse once left-to-right
        // with the stages (design.md Motion: "stages light ... with the
        // cables pulsing once").
        let cable_color = if !self.status.running {
            theme::LINE
        } else if self.reduce_motion {
            theme::AMBER
        } else if self.stagger {
            let t = (self.stagger_t / STAGGER_DURATION).clamp(0.0, 1.0);
            let pulse = (t * std::f32::consts::PI).sin();
            lerp_color(theme::LINE, theme::AMBER, pulse)
        } else if hot {
            let t = self.last_tick.elapsed().as_secs_f32();
            let pulse = 0.5 + 0.5 * (t * CABLE_PULSE_HZ * 2.0 * std::f32::consts::PI).sin();
            lerp_color(theme::LINE, theme::AMBER, pulse)
        } else {
            theme::DIM
        };
        let cable = |color| {
            canvas::Canvas::new(CablePulse { color })
                .width(Length::Fixed(12.0))
                .height(Length::Fixed(44.0))
        };

        // ONE bordered hero panel containing the three equal stage cards
        // joined by cable canvases (design.md "The Chain"). All spacing is
        // on the 4 px grid.
        container(
            row![
                mic_card,
                cable(cable_color),
                link_card,
                cable(cable_color),
                console_card,
            ]
            .spacing(8)
            .align_y(alignment::Vertical::Center)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .padding(12)
        .style(theme::stage_card)
        .into()
    }

    fn stage_card<'a>(
        &self,
        label: &'a str,
        body: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        container(
            column![
                text(label)
                    .font(theme::FONT_LABEL)
                    .size(10)
                    .color(theme::DIM),
                // Equal-height body region: every card's instrument is
                // centered within the same footprint so the three cards
                // render at identical sizes.
                container(body.into())
                    .width(Length::Fill)
                    .height(Length::Fixed(Self::STAGE_BODY_HEIGHT))
                    .align_x(alignment::Horizontal::Center)
                    .align_y(alignment::Vertical::Center),
            ]
            .spacing(4)
            .align_x(alignment::Horizontal::Center),
        )
        .width(Length::FillPortion(1))
        .padding(8)
        .style(theme::stage_card)
        .into()
    }

    // ---- VU ladder ----

    fn vu_ladder<'a>(&'a self) -> Element<'a, Message> {
        let level = self.vu.level();
        // Peak-hold: a single brighter segment at the recent max, decaying
        // at a third of the reading's rate (design.md VU ladder spec).
        let hold = self.vu.hold_segments();
        let ladder = canvas::Canvas::new(VuLadder { lit: vu::vu_segments(level), hold })
            .width(Length::Fill)
            .height(Length::Fixed(128.0));

        row![
            ladder,
            text("VU PEAK")
                .font(theme::FONT_LABEL)
                .size(10)
                .color(theme::DIM),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center)
        .into()
    }

    // ---- Big circular ON AIR toggle ----

    fn air_toggle<'a>(&'a self) -> Element<'a, Message> {
        let live = self.status.running;
        let label = if live { "ON AIR" } else { "STANDBY" };
        // Press pulse: 1.0 right after the press, 0.0 once PRESS_PUSLE_MS
        // elapses (rendered as a brief brightening of the halo; the 200 ms
        // tick gives it a short two-frame flash, per design.md 150-250 ms).
        let pulse = self
            .air_pulse
            .map_or(0.0, |t0| 1.0 - (t0.elapsed().as_secs_f32() / PRESS_PULSE_MS).clamp(0.0, 1.0));

        let dot = canvas::Canvas::new(AirDot {
            live,
            color: if live { theme::TALLY } else { theme::LINE },
        })
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0));

        let content = column![
            dot,
            text(label)
                .font(theme::FONT_HEADER)
                .size(14)
                .color(if live { theme::CREAM } else { theme::DIM }),
        ]
        .spacing(8)
        .align_x(alignment::Horizontal::Center);

        button(
            // Stack: a soft amber backlight disc under the dot + label. The
            // disc is the "glow" (subtle, alpha-blended, no bloom); iced
            // 0.13 Stack has no alignment, so both layers are Fill-sized
            // and the content is centered by the inner container.
            Stack::with_children(vec![
                canvas::Canvas::new(AirHalo { live, pulse })
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                container(content)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(alignment::Horizontal::Center)
                    .align_y(alignment::Vertical::Center)
                    .into(),
            ])
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .on_press(Message::ToggleAir)
        // Zero padding so the halo disc can reach the ring (the default
        // 5/10 px button padding would shrink it off-center).
        .padding(0)
        .style(move |t, s| theme::air_button(t, s, live))
        .width(Length::Fixed(100.0))
        .height(Length::Fixed(100.0))
        .into()
    }

    // ---- Status line ----

    fn status_line<'a>(&'a self) -> Element<'a, Message> {
        let transport = self.engine_cfg.transport.as_str().to_uppercase();
        let codec = self.engine_cfg.codec.as_str().to_uppercase();
        let bind = self.engine_cfg.bind;
        let port = self.engine_cfg.port;
        // Mono, dim, single line: never wraps, truncates with an ellipsis
        // (design.md layout sketch; iced 0.13 has no Text ellipsis, so the
        // truncation is done here with a conservative char budget).
        let s = truncate_with_ellipsis(
            &format!("{transport} · {bind}:{port} · {codec}"),
            STATUS_LINE_MAX_CHARS,
        );
        text(s)
            .font(theme::FONT_MONO)
            .size(11)
            .color(theme::DIM)
            .wrapping(iced::widget::text::Wrapping::None)
            .into()
    }

    // ---- Settings slide-over panel ----

    fn settings_panel<'a>(&'a self) -> Element<'a, Message> {
        let port_valid = self.port_input.parse::<u16>().is_ok_and(|p| p >= 1);

        // NOTE (iced 0.13 layout): every widget inside this column must have
        // a SHRINK main-axis (vertical) size hint. `Row`/`Column::push`
        // propagates child size hints up (`Length::enclose`), so a
        // `Space(Fill, Fill)` here made the whole column Fill-height — which
        // (a) panics `Scrollable::validate` in debug builds
        // ("scrollable content must not fill its vertical scrolling axis")
        // and (b) lays the content out at ~f32::MAX height in release,
        // leaving the panel blank. The header uses a width-only spacer.
        let content = column![
            row![
                text("SETTINGS")
                    .font(theme::FONT_HEADER)
                    .size(16)
                    .color(theme::CREAM),
                Space::with_width(Length::Fill),
                button(
                    text("×")
                        .font(theme::FONT_HEADER)
                        .size(18)
                        .color(theme::DIM)
                )
                .on_press(Message::ToggleSettings)
                .style(theme::flat_button),
            ],
            Space::new(8.0, 8.0),
            text("PORT")
                .font(theme::FONT_LABEL)
                .size(10)
                .color(theme::DIM),
            text_input("41700", &self.port_input)
                .on_input(Message::PortChanged)
                .style(theme::settings_input)
                .font(theme::FONT_MONO)
                .size(14),
            {
                let hint: Element<Message> = if port_valid {
                    Space::new(0.0, 0.0).into()
                } else {
                    text("Port must be 1-65535")
                        .font(theme::FONT_MONO)
                        .size(10)
                        .color(theme::TALLY)
                        .into()
                };
                hint
            },
            Space::new(4.0, 4.0),
            text("BIND ADDRESS")
                .font(theme::FONT_LABEL)
                .size(10)
                .color(theme::DIM),
            pick_list(
                self.bind_options.as_slice(),
                Some(&self.bind_input),
                Message::BindSelected,
            )
            .font(theme::FONT_MONO)
            .text_size(13)
            .style(theme::settings_pick_list),
            Space::new(4.0, 4.0),
            text("CODEC")
                .font(theme::FONT_LABEL)
                .size(10)
                .color(theme::DIM),
            row![
                codec_chip("AUTO", CodecMode::Auto, self.codec),
                codec_chip("PCM", CodecMode::Pcm, self.codec),
                codec_chip("OPUS", CodecMode::Opus, self.codec),
            ]
            .spacing(8),
            Space::new(4.0, 4.0),
            // Label in Chakra, value in Plex Mono: design.md binds every
            // numeric readout to IBM Plex Mono.
            row![
                text("JITTER TARGET")
                    .font(theme::FONT_LABEL)
                    .size(10)
                    .color(theme::DIM),
                Space::with_width(Length::Fill),
                text(format!("{:.1} ms", self.target_ms))
                    .font(theme::FONT_MONO_MEDIUM)
                    .size(12)
                    .color(theme::CREAM),
            ]
            .align_y(alignment::Vertical::Center),
            slider(5.0..=20.0, self.target_ms, Message::TargetChanged).step(0.5_f32),
            Space::new(4.0, 4.0),
            toggler(self.autostart)
                .label("AUTOSTART")
                .on_toggle(Message::AutostartToggled)
                .font(theme::FONT_LABEL)
                .text_size(12),
            toggler(self.start_minimized)
                .label("START MINIMIZED")
                .on_toggle(Message::MinimizedToggled)
                .font(theme::FONT_LABEL)
                .text_size(12),
            toggler(self.reduce_motion)
                .label("REDUCED MOTION")
                .on_toggle(Message::MotionToggled)
                .font(theme::FONT_LABEL)
                .text_size(12),
            Space::new(12.0, 12.0),
            button(
                text("Save settings")
                    .font(theme::FONT_LABEL)
                    .size(14)
                    .color(theme::CREAM),
            )
            .on_press(Message::SaveSettings)
            .style(theme::engraved_button)
            .width(Length::Fill),
        ]
        .spacing(8);

        container(
            // The scrollable must be explicitly Fill-sized so its viewport
            // matches the panel and the content column actually scrolls.
            scrollable(content)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(16)
        .width(300.0)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(theme::INK)),
            border: iced::Border {
                radius: 0.0.into(),
                width: 1.0,
                color: theme::LINE,
            },
            ..container::Style::default()
        })
        .into()
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn codec_chip<'a>(label: &'a str, value: CodecMode, current: CodecMode) -> Element<'a, Message> {
    let selected = value == current;
    let (fg, border) = if selected {
        (theme::CREAM, theme::AMBER)
    } else {
        (theme::DIM, theme::LINE)
    };
    button(text(label).font(theme::FONT_LABEL).size(12).color(fg))
        .on_press(Message::CodecSelected(value))
        .style(move |_t, _s| button::Style {
            background: Some(iced::Background::Color(theme::PANEL)),
            border: iced::Border {
                radius: 2.0.into(),
                width: if selected { 2.0 } else { 1.0 },
                color: border,
            },
            text_color: fg,
            ..button::Style::default()
        })
        .into()
}

fn engine_cmd(engine: &EngineHandle, cmd: EngineCommand) -> Task<Message> {
    let sender = engine.cmd();
    Task::perform(
        async move {
            let _ = sender.send(cmd).await;
        },
        |_| Message::CmdSent,
    )
}

/// Linear interpolation between two colors.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

/// Max chars for the status line. IBM Plex Mono advances 0.6 em per glyph:
/// at 11 px that is 6.6 px/char; the narrowest content column (420 px window
/// minus 2*16 px root padding) fits 388/6.6 ≈ 58 chars. 56 leaves a little
/// headroom for the ellipsis and subpixel rounding. The longest real string
/// ("UDP · 255.255.255.255:41700 · AUTO") is 34 chars, so this only triggers
/// as a guard against wrapping, which is the point.
const STATUS_LINE_MAX_CHARS: usize = 56;

/// Truncate `s` to at most `max` characters, appending a Unicode ellipsis
/// when cut. iced 0.13's `Text` has `Wrapping::None` but no built-in
/// ellipsis, so the status line truncates here (design.md: status line is a
/// single mono line, never wrapped).
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}'); // U+2026 HORIZONTAL ELLIPSIS — audited in all fonts
    out
}

/// Enumerate local IP addresses for the bind dropdown, plus "0.0.0.0" and
/// the current value (so a saved address always appears in the list).
fn get_local_ips(current: &str) -> Vec<String> {
    let mut ips: Vec<String> = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in &ifaces {
            let ip = iface.addr.ip().to_string();
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    ips.sort();
    for extra in ["0.0.0.0", current] {
        if !ips.iter().any(|ip| ip == extra) {
            ips.push(extra.to_string());
        }
    }
    ips
}

// ---------------------------------------------------------------------------
// Canvas programs
// ---------------------------------------------------------------------------

/// Lamp dot in the header (tally red while running, line color idle).
struct LampDot {
    color: Color,
}

impl<Message> Program<Message> for LampDot {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let r = bounds.width.min(bounds.height) / 2.0 - 1.0;
        frame.fill(&Path::circle(frame.center(), r), self.color);
        vec![frame.into_geometry()]
    }
}

/// Amber dot on the menu button when there are unsaved changes.
struct DirtyDot;

impl<Message> Program<Message> for DirtyDot {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let r = bounds.width.min(bounds.height) / 2.0 - 0.5;
        frame.fill(&Path::circle(frame.center(), r), theme::AMBER);
        vec![frame.into_geometry()]
    }
}

/// Canvas-drawn hamburger icon (three 2 px cream lines, square caps for the
/// machined look). The "≡" glyph (U+2261) is absent from Chakra Petch, so
/// icons must never rely on glyph coverage.
struct MenuIcon;

impl<Message> Program<Message> for MenuIcon {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        let stroke = Stroke {
            width: 2.0,
            style: canvas::Style::Solid(theme::CREAM),
            ..Stroke::default()
        };
        for frac in [0.25, 0.5, 0.75] {
            let y = h * frac;
            frame.stroke(
                &Path::line(Point::new(w * 0.15, y), Point::new(w * 0.85, y)),
                stroke,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// MIC level ring: line track, amber/cream arc proportional to the smoothed
/// capture level; tally when clipping.
struct LevelRing {
    level: f32, // 0..=1
    hot: bool,
    lit: bool,
}

impl<Message> Program<Message> for LevelRing {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = frame.center();
        let r = bounds.width.min(bounds.height) / 2.0 - 4.0;
        let track = Path::circle(center, r);
        let thickness = 3.0;

        frame.stroke(
            &track,
            Stroke {
                width: thickness,
                style: canvas::Style::Solid(theme::LINE),
                ..Stroke::default()
            },
        );

        if self.level > 0.0 && self.lit {
            let arc_color = if self.level >= vu::db_to_level(-3.0) {
                theme::TALLY
            } else if self.hot {
                theme::AMBER
            } else {
                theme::DIM
            };
            // Arc starts at 12 o'clock and sweeps clockwise.
            let start = -std::f32::consts::FRAC_PI_2;
            let end = start + 2.0 * std::f32::consts::PI * self.level.min(1.0);
            let arc = Path::new(|b| {
                b.arc(canvas::path::Arc {
                    center,
                    radius: r,
                    start_angle: Radians(start),
                    end_angle: Radians(end),
                });
            });
            frame.stroke(
                &arc,
                Stroke {
                    width: thickness,
                    style: canvas::Style::Solid(arc_color),
                    line_cap: canvas::LineCap::Round,
                    ..Stroke::default()
                },
            );
        }

        vec![frame.into_geometry()]
    }
}

/// The cable segment between chain stages: a short vertical line whose color
/// is animated by the caller (pulse while streaming).
struct CablePulse {
    color: Color,
}

impl<Message> Program<Message> for CablePulse {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let cx = frame.center().x;
        let line = Path::line(Point::new(cx, 4.0), Point::new(cx, bounds.height - 4.0));
        frame.stroke(
            &line,
            Stroke {
                width: 2.0,
                style: canvas::Style::Solid(self.color),
                line_cap: canvas::LineCap::Round,
                ..Stroke::default()
            },
        );
        vec![frame.into_geometry()]
    }
}

/// Small horizontal fill gauge under the CONSOLE value (jitter-buffer fill).
struct FillBar {
    fraction: f32,
    hot: bool,
}

impl<Message> Program<Message> for FillBar {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Track.
        let track = Path::rectangle(Point::new(0.0, 0.0), bounds.size());
        frame.fill(&track, if self.hot { theme::PANEL } else { theme::LINE });
        // Fill.
        let fill_w = bounds.width * self.fraction;
        if fill_w > 0.0 {
            let fill = Path::rectangle(Point::new(0.0, 0.0), Size::new(fill_w, bounds.height));
            frame.fill(&fill, theme::AMBER);
        }
        vec![frame.into_geometry()]
    }
}

/// The soft backlight behind the ON AIR ring: a subtle amber disc while
/// live (alpha ~0.10, no bloom) that brightens briefly on every press
/// (`pulse` 1.0 -> 0.0 over ~200 ms). Alpha-blended amber is the one
/// sanctioned exception to the palette (design.md glow alphas).
struct AirHalo {
    live: bool,
    pulse: f32,
}

impl<Message> Program<Message> for AirHalo {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Backlight when live; the press pulse adds a short-lived bump.
        let alpha = if self.live { 0.10 } else { 0.0 } + self.pulse * 0.22;
        if alpha > 0.0 {
            let r = bounds.width.min(bounds.height) / 2.0 - 1.0;
            frame.fill(
                &Path::circle(frame.center(), r),
                Color::from_rgba(theme::AMBER.r, theme::AMBER.g, theme::AMBER.b, alpha.min(0.35)),
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The tally dot inside the ON AIR toggle: filled red on air, ring while
/// standing by.
struct AirDot {
    live: bool,
    color: Color,
}

impl<Message> Program<Message> for AirDot {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let circle = Path::circle(frame.center(), bounds.width.min(bounds.height) / 2.0 - 1.0);
        if self.live {
            frame.fill(&circle, self.color);
        } else {
            frame.stroke(
                &circle,
                Stroke {
                    width: 1.5,
                    style: canvas::Style::Solid(self.color),
                    ..Stroke::default()
                },
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The VU ladder: 24 horizontal segments stacked bottom-up; bottom 18 cream,
/// next 3 amber, top 3 tally red (clip); unlit = line. `hold` is the
/// peak-hold segment count: the single segment just above the lit ones is
/// drawn cream as the "recent max" marker (decays at a third of the reading
/// rate in `vu::VuBallistics`).
struct VuLadder {
    lit: usize,
    hold: usize,
}

impl<Message> Program<Message> for VuLadder {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Segment geometry: 3 px bars, 2 px gaps, full width, square corners
        // — a machined VU face, not a pill (design.md anti-goals).
        let seg_h = 3.0;
        let gap = 2.0;
        let total_h = vu::SEGMENTS as f32 * (seg_h + gap) - gap;
        let start_y = (bounds.height - total_h) / 2.0;
        let bar_w = bounds.width;

        for i in 0..vu::SEGMENTS {
            let y = start_y + i as f32 * (seg_h + gap);
            // Peak-hold marker: exactly one brighter segment at the recent
            // max, floating above the falling bars (cream, the VU-face
            // color). The zone colors below it keep priority.
            let color = if i < self.lit {
                if i >= vu::RED_START {
                    theme::TALLY
                } else if i >= vu::AMBER_START {
                    theme::AMBER
                } else {
                    theme::CREAM
                }
            } else if i == self.hold.saturating_sub(1) && self.hold > self.lit {
                theme::CREAM
            } else {
                theme::LINE
            };
            let rect = Path::rectangle(Point::new(0.0, y), Size::new(bar_w, seg_h));
            frame.fill(&rect, color);
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an app with real engine + temp config path (no tray).
    fn test_app(config: Config) -> App {
        let flags = Flags {
            engine: openay_server::spawn_engine(None),
            config,
            config_path: std::env::temp_dir().join("openay-mic-test-config.toml"),
            tray_bus: Arc::new(crate::tray::TrayBus::default()),
            tray: None,
        };
        let (app, _task) = App::new(flags);
        app
    }

    /// Lay the app's view out headlessly with a real tiny-skia renderer.
    fn layout(app: &App, width: f32, height: f32) -> iced_core::layout::Node {
        let renderer = iced::Renderer::new(theme::FONT_MONO, 16.0.into());
        let limits =
            iced_core::layout::Limits::new(iced::Size::ZERO, iced::Size::new(width, height));
        let el = app.view();
        let mut tree = iced_core::widget::Tree::new(el.as_widget());
        el.as_widget().layout(&mut tree, &renderer, &limits)
    }

    /// Recursively assert no node has an absurd (infinite / huge) size or
    /// position — catches the f32::MAX content regression.
    fn assert_finite_layout(node: &iced_core::layout::Node) {
        let b = node.bounds();
        for v in [b.x, b.y, b.width, b.height] {
            assert!(
                v.is_finite() && v.abs() < 1.0e6,
                "non-finite layout: {b:?}"
            );
        }
        for c in node.children() {
            assert_finite_layout(c);
        }
    }

    /// Count all nodes in a subtree.
    fn count_nodes(node: &iced_core::layout::Node) -> usize {
        1 + node.children().iter().map(count_nodes).sum::<usize>()
    }

    // -----------------------------------------------------------------------
    // BUG 2 regression: the settings panel must build without tripping
    // Scrollable::validate's debug_assert (the content column became
    // Fill-height through Space(Fill, Fill) size-hint propagation).
    // -----------------------------------------------------------------------

    #[test]
    fn settings_panel_builds_without_panicking() {
        let mut app = test_app(Config::default());
        app.settings_open = true;
        let _el = app.view();
    }

    // -----------------------------------------------------------------------
    // BUG 1 regression: main layout shape matches design.md
    // (header / hero panel / VU row / centered toggle / status line).
    // -----------------------------------------------------------------------

    #[test]
    fn layout_main_matches_design() {
        let app = test_app(Config::default());
        let node = layout(&app, 460.0, 600.0);
        assert_finite_layout(&node);

        assert_eq!(node.size().width, 460.0);
        assert_eq!(node.size().height, 600.0);
        let children = node.children();
        assert_eq!(children.len(), 6, "root column must have 6 children");

        // 0: header — compact (NOT a Fill-height band) and full width.
        let header = children[0].bounds();
        assert!(
            header.height < 60.0,
            "header must be compact, got height {}",
            header.height
        );
        assert!(header.width > 400.0, "header must span the window");

        // 1: Chain hero — ONE full-width panel.
        let hero = children[1].bounds();
        assert!(hero.width > 400.0, "hero panel must span the window");

        // 2: VU ladder — its own full-width row BELOW the hero, no overlap.
        let vu = children[2].bounds();
        assert!(vu.width > 400.0, "VU row must span the window");
        assert!(
            vu.y >= hero.y + hero.height - 1.0,
            "VU must sit below the hero card, hero={hero:?} vu={vu:?}"
        );

        // 3: ON AIR toggle — 100x100 and horizontally centered.
        let toggle = children[3].bounds();
        assert_eq!(toggle.width, 100.0);
        assert_eq!(toggle.height, 100.0);
        let center = toggle.x + toggle.width / 2.0;
        assert!(
            (center - 230.0).abs() <= 8.0,
            "toggle must be centered, center={center}"
        );

        // 5: status line — pinned to the bottom.
        let status = children[5].bounds();
        assert!(
            status.y + status.height > 560.0,
            "status line must sit near the bottom, got {status:?}"
        );
    }

    #[test]
    fn layout_main_fits_minimum_window() {
        // 420x520 (the declared minimum) must not overlap either.
        let app = test_app(Config::default());
        let node = layout(&app, 420.0, 520.0);
        assert_finite_layout(&node);
        let children = node.children();
        let hero = children[1].bounds();
        let vu = children[2].bounds();
        assert!(
            vu.y >= hero.y + hero.height - 1.0,
            "VU must sit below the hero card at min size"
        );
    }

    /// At the 420 px minimum the widest stage readouts ("1314 LOST" at
    /// 12 px, "10.0 ms" at 20 px IBM Plex Mono) must stay inside their
    /// cards — no overflow, no wrap (design.md Chain contract).
    #[test]
    fn stage_values_fit_inside_cards_at_min_window() {
        let app = test_app(Config::default());
        let node = layout(&app, 420.0, 520.0);
        let hero = &node.children()[1];
        let hero_b = hero.bounds();
        let row = &hero.children()[0];
        let row_b = row.bounds();
        let row_abs = iced::Point::new(hero_b.x + row_b.x, hero_b.y + row_b.y);

        // Cards at row indices 0 (MIC), 2 (LINK), 4 (CONSOLE); cables at
        // 1 and 3. Node bounds are parent-relative, so offsets accumulate.
        for &card_idx in &[0, 2, 4] {
            let card = &row.children()[card_idx];
            let cb = card.bounds();
            let card_abs =
                iced::Rectangle::new(iced::Point::new(row_abs.x + cb.x, row_abs.y + cb.y), cb.size());
            // Content box: the card padding is 8 px on every side.
            let content = iced::Rectangle {
                x: card_abs.x + 8.0,
                y: card_abs.y + 8.0,
                width: card_abs.width - 16.0,
                height: card_abs.height - 16.0,
            };
            // card -> column -> [label, body]; body -> inner content.
            let column = &card.children()[0];
            let col_abs =
                iced::Point::new(card_abs.x + column.bounds().x, card_abs.y + column.bounds().y);
            let body = &column.children()[1];
            let body_abs =
                iced::Point::new(col_abs.x + body.bounds().x, col_abs.y + body.bounds().y);
            let inner = &body.children()[0];
            let inner_abs =
                iced::Point::new(body_abs.x + inner.bounds().x, body_abs.y + inner.bounds().y);
            for leaf in inner.children() {
                let lb = leaf.bounds();
                let leaf_abs = iced::Rectangle::new(
                    iced::Point::new(inner_abs.x + lb.x, inner_abs.y + lb.y),
                    lb.size(),
                );
                assert!(
                    leaf_abs.x >= content.x - 1.0
                        && leaf_abs.x + leaf_abs.width <= content.x + content.width + 1.0,
                    "stage value escapes its card at min width: {leaf_abs:?} \
                     vs content box {content:?}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Status line truncation: mono, single line, ellipsis instead of wrap.
    // -----------------------------------------------------------------------

    #[test]
    fn status_line_truncates_with_ellipsis() {
        let short = "UDP · 0.0.0.0:41700 · AUTO";
        assert_eq!(truncate_with_ellipsis(short, 56), short, "fits: untouched");

        let long = "UDP · 255.255.255.255:41700 · OPUS";
        let cut = truncate_with_ellipsis(long, 20);
        assert_eq!(cut, "UDP · 255.255.255.2\u{2026}", "cut at 19 chars + ellipsis");
        assert_eq!(cut.chars().count(), 20, "max chars including ellipsis");
        assert!(cut.ends_with('\u{2026}'), "ends with a real ellipsis");

        let empty = truncate_with_ellipsis("", 3);
        assert_eq!(empty, "");
        assert_eq!(truncate_with_ellipsis("abc", 3), "abc");
        assert_eq!(truncate_with_ellipsis("abcd", 3), "ab\u{2026}");
    }

    // -----------------------------------------------------------------------
    // BUG 2 regression: the settings overlay geometry — right-pinned 300px
    // panel, full height, rich content, no f32::MAX content.
    // -----------------------------------------------------------------------

    #[test]
    fn layout_settings_overlay_matches_design() {
        let app = test_app(Config::default());
        let mut app = app;
        app.settings_open = true;
        let node = layout(&app, 460.0, 600.0);
        assert_finite_layout(&node);

        // Root is the Stack: [content, scrim, settings row].
        assert_eq!(node.children().len(), 3);
        let row = &node.children()[2];
        assert_eq!(row.children().len(), 2);
        let panel = row.children()[1].bounds();
        assert_eq!(panel.width, 300.0, "panel must be 300 px wide");
        assert_eq!(panel.height, 600.0, "panel must span the full height");
        assert!(
            (panel.x - 160.0).abs() <= 1.0,
            "panel must be pinned to the right edge, x={}",
            panel.x
        );

        // The panel subtree must be rich (all controls present) and its
        // content must NOT be laid out at an astronomical offset (the
        // pre-fix behavior was content at ~f32::MAX due to Fill-height
        // propagation into the scrollable).
        let panel_subtree = count_nodes(&row.children()[1]);
        assert!(
            panel_subtree > 30,
            "settings content missing: only {panel_subtree} nodes"
        );
    }

    // -----------------------------------------------------------------------
    // Settings form state machine: open/close, dirty tracking, save applies
    // and persists, invalid input is rejected, sliders are clamped.
    // -----------------------------------------------------------------------

    #[test]
    fn settings_form_open_close_and_dirty_tracking() {
        let mut app = test_app(Config::default());
        assert!(!app.settings_open);

        let _ = app.update(Message::ToggleSettings);
        assert!(app.settings_open);
        let _ = app.update(Message::ToggleSettings);
        assert!(!app.settings_open, "toggle closes the panel");

        assert!(!app.dirty);
        let _ = app.update(Message::PortChanged("43210".into()));
        assert!(app.dirty);
        let _ = app.update(Message::TargetChanged(12.5));
        assert!(app.dirty);
        let _ = app.update(Message::MotionToggled(true));
        assert!(app.dirty);
    }

    #[test]
    fn save_applies_config_and_clears_dirty() {
        let mut app = test_app(Config::default());
        let _ = app.update(Message::PortChanged("43210".into()));
        let _ = app.update(Message::BindSelected("127.0.0.1".into()));
        let _ = app.update(Message::CodecSelected(CodecMode::Opus));
        let _ = app.update(Message::TargetChanged(12.5));
        let _ = app.update(Message::AutostartToggled(true));
        let _ = app.update(Message::MotionToggled(true));
        assert!(app.dirty);

        let _ = app.update(Message::SaveSettings);
        assert!(!app.dirty, "save must clear dirty");
        assert_eq!(app.config.port, 43210);
        assert_eq!(app.config.bind, "127.0.0.1");
        assert_eq!(app.config.codec, "opus");
        assert_eq!(app.config.target_ms, 12.5);
        assert!(app.config.autostart);
        assert!(app.config.reduce_motion);
        // The engine config mirrors the saved settings.
        assert_eq!(app.engine_cfg.port, 43210);
        assert_eq!(app.engine_cfg.codec, CodecMode::Opus);
    }

    #[test]
    fn save_rejects_invalid_port_and_clamps_slider() {
        let mut app = test_app(Config::default());
        // Empty and zero ports must not apply or clear dirty.
        for bad in ["", "0", "abc", "99999"] {
            let _ = app.update(Message::PortChanged(bad.into()));
            assert!(app.dirty);
            let _ = app.update(Message::SaveSettings);
            assert!(app.dirty, "invalid port {bad:?} must not save");
            assert_eq!(app.config.port, config::DEFAULT_PORT);
        }

        // Out-of-range jitter target is clamped to the engine limits (5..=20).
        let _ = app.update(Message::TargetChanged(100.0));
        let _ = app.update(Message::PortChanged("41700".into()));
        let _ = app.update(Message::SaveSettings);
        assert_eq!(app.config.target_ms, openay_server::MAX_PREBUFFER_MS);
        assert!(!app.dirty);
    }

    #[test]
    fn settings_apply_to_engine_config() {
        let mut app = test_app(Config::default());
        let _ = app.update(Message::CodecSelected(CodecMode::Pcm));
        let _ = app.update(Message::TargetChanged(7.5));
        let _ = app.update(Message::SaveSettings);
        assert_eq!(app.engine_cfg.codec, CodecMode::Pcm);
        assert_eq!(app.engine_cfg.target_ms, 7.5);
        assert_eq!(app.engine_cfg.transport, openay_server::Transport::Udp);
    }

    // -----------------------------------------------------------------------
    // Reduced-motion propagation: the power-on stagger is skipped.
    // -----------------------------------------------------------------------

    #[test]
    fn reduced_motion_skips_power_on_stagger() {
        let mut app = test_app(Config::default());
        app.reduce_motion = false;
        let _ = app.toggle_air();
        assert!(app.stagger, "default: pressing the lamp starts the stagger");

        app.reduce_motion = true;
        app.stagger = false;
        let _ = app.toggle_air();
        assert!(
            !app.stagger,
            "reduced motion must suppress the power-on stagger"
        );
    }

    #[test]
    fn motion_toggle_propagates_and_marks_dirty() {
        let mut app = test_app(Config::default());
        assert!(!app.reduce_motion);
        let _ = app.update(Message::MotionToggled(true));
        assert!(app.reduce_motion);
        assert!(app.dirty);
    }

    // -----------------------------------------------------------------------
    // Glyph safety: every user-visible string must be renderable with the
    // bundled fonts (audited against their cmaps).
    // -----------------------------------------------------------------------

    /// Every literal user-visible string in the GUI. Keep in sync with the
    /// `view()` code — the test below guarantees each char is in the
    /// audited allowlist.
    const USER_FACING_STRINGS: &[&str] = &[
        "OPENAY MIC",
        "MIC",
        "LINK",
        "CONSOLE",
        "VU PEAK",
        "ON AIR",
        "STANDBY",
        "SETTINGS",
        "×", // close button; present in all four bundled fonts (cmap audit)
        "PORT",
        "41700", // port placeholder
        "Port must be 1-65535",
        "BIND ADDRESS",
        "CODEC",
        "AUTO",
        "PCM",
        "OPUS",
        "JITTER TARGET",
        "{:.1} ms",
        "AUTOSTART",
        "START MINIMIZED",
        "REDUCED MOTION",
        "Save settings",
        "{:.0}/s",
        "{} LOST",
        "{transport} · {bind}:{port} · {codec}",
    ];

    /// Characters verified (via cmap parsing) to exist in every bundled
    /// font, in addition to printable ASCII.
    const EXTRA_ALLOWED: &[char] = &['·', '×', '\u{2026}'];

    fn is_allowed_glyph(c: char) -> bool {
        c.is_ascii_graphic() || c == ' ' || EXTRA_ALLOWED.contains(&c)
    }

    #[test]
    fn every_user_facing_string_is_glyph_safe() {
        for s in USER_FACING_STRINGS {
            for c in s.chars() {
                assert!(
                    is_allowed_glyph(c),
                    "char U+{:04X} ({c:?}) in string {s:?} is outside the \
                     audited glyph allowlist",
                    c as u32
                );
            }
        }
    }

    /// Minimal TrueType cmap reader (format 4, Windows Unicode BMP) — enough
    /// to verify that the non-ASCII allowlist chars really exist in the
    /// bundled TTFs at test time.
    fn font_has_glyph(font: &[u8], cp: u32) -> bool {
        let be16 = |o: usize| u16::from_be_bytes([font[o], font[o + 1]]);
        let be32 = |o: usize| u32::from_be_bytes([font[o], font[o + 1], font[o + 2], font[o + 3]]);
        let num_tables = be16(4) as usize;
        let mut cmap_off = None;
        for i in 0..num_tables {
            let e = 12 + 16 * i;
            if &font[e..e + 4] == b"cmap" {
                cmap_off = Some(be32(e + 8) as usize);
            }
        }
        let cmap_off = cmap_off.expect("font has a cmap table");
        let n = be16(cmap_off + 2) as usize;
        let mut sub = None;
        for i in 0..n {
            let r = cmap_off + 4 + 8 * i;
            if be16(r) == 3 && be16(r + 2) == 1 {
                sub = Some(cmap_off + be32(r + 4) as usize);
            }
        }
        let sub = sub.expect("font has a format-4 unicode subtable");
        let seg = be16(sub + 6) as usize / 2;
        let ends_off = sub + 14;
        let starts_off = ends_off + 2 * seg;
        for i in 0..seg {
            let start = be16(starts_off + 2 * i) as u32;
            let end = be16(ends_off + 2 * i) as u32;
            if cp >= start && cp <= end {
                return true;
            }
            if cp < start {
                break;
            }
        }
        false
    }

    #[test]
    fn extra_allowlist_chars_exist_in_bundled_fonts() {
        let fonts = [
            (theme::CHAKRA_SEMIBOLD, "ChakraPetch-SemiBold"),
            (theme::CHAKRA_MEDIUM, "ChakraPetch-Medium"),
            (theme::PLEX_REGULAR, "IBMPlexMono-Regular"),
            (theme::PLEX_MEDIUM, "IBMPlexMono-Medium"),
        ];
        for (font, name) in fonts {
            for c in EXTRA_ALLOWED {
                assert!(
                    font_has_glyph(font, *c as u32),
                    "U+{:04X} ({c}) missing from {name}",
                    *c as u32
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Canvas containment: the MIC ring, cables, VU ladder, lamp dot and the
    // toggle dot must all be laid out inside their declared sizes (the stray
    // floating circle was the toggle dot escaping its 100x100 button).
    // -----------------------------------------------------------------------

    #[test]
    fn toggle_dot_is_contained_inside_button() {
        let app = test_app(Config::default());
        let node = layout(&app, 460.0, 600.0);
        let toggle = &node.children()[3];
        // Node bounds are relative to the parent, so every child must fit
        // inside its parent's size (the pre-fix toggle content was pinned to
        // the button's top-left corner, never centered).
        fn max_extent(node: &iced_core::layout::Node, parent: iced::Size) {
            for c in node.children() {
                let cb = c.bounds();
                assert!(
                    cb.x >= -0.01
                        && cb.y >= -0.01
                        && cb.x + cb.width <= parent.width + 0.01
                        && cb.y + cb.height <= parent.height + 0.01,
                    "node {cb:?} escapes its parent ({parent:?})"
                );
                max_extent(c, cb.size());
            }
        }
        max_extent(toggle, toggle.size());
    }
}

