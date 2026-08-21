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
                success: Color::from_rgb(0.34, 0.66, 0.46),
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
            Message::CloseRequested(id) => {
                self.window_id = Some(id);
                // Hide to tray instead of closing.
                window::change_mode(id, window::Mode::Hidden)
            }
            Message::WindowId(id) => {
                self.window_id = id;
                Task::none()
            }
            Message::CmdSent => Task::none(),
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

        // Tray requests.
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if self.tray_bus.take_quit() {
            if let Some(id) = self.window_id {
                tasks.push(window::close(id));
            }
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
        let content = column![
            self.header(),
            self.hero_chain(),
            self.vu_ladder(),
            Space::new(6.0, 6.0),
            self.air_toggle(),
            Space::new(Length::Fill, Length::Fill),
            self.status_line(),
        ]
        .spacing(10)
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill);

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
                // Settings panel pinned to the right edge.
                row![
                    Space::new(Length::Fill, Length::Fill),
                    self.settings_panel()
                ]
                .into(),
            ])
            .into()
        } else {
            content.into()
        }
    }

    // ---- Header: lamp dot + wordmark + menu button ----

    fn header<'a>(&'a self) -> Element<'a, Message> {
        let lamp_color = if self.status.running {
            theme::TALLY
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
                text("≡")
                    .font(theme::FONT_HEADER)
                    .size(18)
                    .color(theme::DIM),
                menu_dot,
            ]
            .spacing(3)
            .align_y(alignment::Vertical::Center),
        )
        .on_press(Message::ToggleSettings)
        .style(theme::flat_button);

        row![
            lamp,
            Space::new(8.0, 8.0),
            text("OPENAY MIC")
                .font(theme::FONT_HEADER)
                .size(16)
                .color(theme::CREAM),
            Space::new(Length::Fill, Length::Fill),
            menu_btn,
        ]
        .align_y(alignment::Vertical::Center)
        .into()
    }

    // ---- The Chain (hero card) ----

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
                    .color(if self.status.lost > 0 {
                        theme::TALLY
                    } else {
                        value_color(lit(1))
                    }),
            ]
            .spacing(2)
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
        let cable_color = if !self.status.running {
            theme::LINE
        } else if self.reduce_motion {
            theme::AMBER
        } else if hot {
            let t = self.last_tick.elapsed().as_secs_f32();
            let pulse = 0.5 + 0.5 * (t * CABLE_PULSE_HZ * 2.0 * std::f32::consts::PI).sin();
            lerp_color(theme::LINE, theme::AMBER, pulse)
        } else {
            theme::DIM
        };
        let cable = canvas::Canvas::new(CablePulse { color: cable_color })
            .width(Length::Fixed(14.0))
            .height(Length::Fixed(44.0));

        row![
            mic_card,
            cable,
            link_card,
            canvas::Canvas::new(CablePulse { color: cable_color })
                .width(Length::Fixed(14.0))
                .height(Length::Fixed(44.0)),
            console_card,
        ]
        .spacing(6)
        .align_y(alignment::Vertical::Center)
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
                body.into(),
            ]
            .spacing(4)
            .align_x(alignment::Horizontal::Center),
        )
        .padding(8)
        .style(theme::stage_card)
        .into()
    }

    // ---- VU ladder ----

    fn vu_ladder<'a>(&'a self) -> Element<'a, Message> {
        let level = self.vu.level();
        let ladder = canvas::Canvas::new(VuLadder {
            lit: vu::vu_segments(level),
        })
        .width(Length::Fill)
        .height(Length::Fixed(128.0));

        row![
            ladder,
            text("VU PEAK")
                .font(theme::FONT_LABEL)
                .size(10)
                .color(theme::DIM),
        ]
        .spacing(10)
        .align_y(alignment::Vertical::Center)
        .into()
    }

    // ---- Big circular ON AIR toggle ----

    fn air_toggle<'a>(&'a self) -> Element<'a, Message> {
        let live = self.status.running;
        let label = if live { "ON AIR" } else { "STANDBY" };

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
        .spacing(6)
        .align_x(alignment::Horizontal::Center);

        button(content)
            .on_press(Message::ToggleAir)
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
        text(format!("{transport} · {bind}:{port} · {codec}"))
            .font(theme::FONT_MONO)
            .size(11)
            .color(theme::DIM)
            .into()
    }

    // ---- Settings slide-over panel ----

    fn settings_panel<'a>(&'a self) -> Element<'a, Message> {
        let port_valid = self.port_input.parse::<u16>().is_ok_and(|p| p >= 1);

        let content = column![
            row![
                text("SETTINGS")
                    .font(theme::FONT_HEADER)
                    .size(16)
                    .color(theme::CREAM),
                Space::new(Length::Fill, Length::Fill),
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
            .text_size(13),
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
            .spacing(6),
            Space::new(4.0, 4.0),
            text(format!("JITTER TARGET   {:.1} ms", self.target_ms))
                .font(theme::FONT_LABEL)
                .size(10)
                .color(theme::DIM),
            slider(5.0..=20.0, self.target_ms, Message::TargetChanged).step(0.5_f32),
            Space::new(4.0, 4.0),
            toggler(self.autostart)
                .label("AUTOSTART")
                .on_toggle(Message::AutostartToggled)
                .text_size(12),
            toggler(self.start_minimized)
                .label("START MINIMIZED")
                .on_toggle(Message::MinimizedToggled)
                .text_size(12),
            toggler(self.reduce_motion)
                .label("REDUCED MOTION")
                .on_toggle(Message::MotionToggled)
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
        .spacing(6);

        container(scrollable(content))
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
/// next 3 amber, top 3 tally red; unlit = line.
struct VuLadder {
    lit: usize,
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
        let seg_h = 3.0;
        let gap = 2.0;
        let total_h = vu::SEGMENTS as f32 * (seg_h + gap) - gap;
        let start_y = (bounds.height - total_h) / 2.0;
        let bar_w = bounds.width;

        for i in 0..vu::SEGMENTS {
            let y = start_y + i as f32 * (seg_h + gap);
            let color = if i < self.lit {
                if i >= vu::RED_START {
                    theme::TALLY
                } else if i >= vu::AMBER_START {
                    theme::AMBER
                } else {
                    theme::CREAM
                }
            } else {
                theme::LINE
            };
            let rect = Path::rectangle(Point::new(0.0, y), Size::new(bar_w, seg_h));
            frame.fill(&rect, color);
        }
        vec![frame.into_geometry()]
    }
}
