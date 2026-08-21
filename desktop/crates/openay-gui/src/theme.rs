//! OpenAY Mic design system — palette and typography from `shared/design.md`
//! ("Studio rack at night"). Every color/type decision below is binding.

use iced::widget::{button, container, pick_list, text_input};
use iced::{Border, Color, Font};

// ---------------------------------------------------------------------------
// Palette (binding, design.md §Palette)
// ---------------------------------------------------------------------------

/// App background — warm near-black, brown-tinted.
pub const INK: Color = Color::from_rgb(0.078, 0.071, 0.059);
/// Raised surfaces/cards.
pub const PANEL: Color = Color::from_rgb(0.118, 0.106, 0.086);
/// Hairlines, segment tracks.
pub const LINE: Color = Color::from_rgb(0.204, 0.188, 0.165);
/// Primary text — illuminated VU-face cream.
pub const CREAM: Color = Color::from_rgb(0.937, 0.902, 0.831);
/// Active/live accents, lit meters.
pub const AMBER: Color = Color::from_rgb(1.0, 0.706, 0.329);
/// ONLY the ON AIR lamp + clip zone.
pub const TALLY: Color = Color::from_rgb(0.898, 0.282, 0.302);
/// Secondary text, disabled.
pub const DIM: Color = Color::from_rgb(0.553, 0.518, 0.467);

/// Subtle vertical shading on panels (machined metal, <= 6% lightness lift):
/// a linear gradient from a hair lighter panel tone at the top to the panel
/// base at the bottom.
pub fn panel_background() -> iced::Background {
    let gradient = iced::Gradient::Linear(
        iced::gradient::Linear::new(std::f32::consts::FRAC_PI_2)
            .add_stop(0.0, Color::from_rgb(0.132, 0.119, 0.097)) // lifted ~4%
            .add_stop(1.0, PANEL),
    );
    iced::Background::Gradient(gradient)
}

// ---------------------------------------------------------------------------
// Typography (binding, design.md §Typography)
// ---------------------------------------------------------------------------

/// Embedded font bytes, loaded at startup via `iced::application().font(..)`.
/// Paths are resolved relative to the crate manifest (`CARGO_MANIFEST_DIR`),
/// which is `desktop/crates/openay-gui`; the fonts live in `shared/fonts/`.
pub const CHAKRA_SEMIBOLD: &[u8] =
    include_bytes!("../../../../shared/fonts/ChakraPetch-SemiBold.ttf");
pub const CHAKRA_MEDIUM: &[u8] = include_bytes!("../../../../shared/fonts/ChakraPetch-Medium.ttf");
pub const PLEX_REGULAR: &[u8] = include_bytes!("../../../../shared/fonts/IBMPlexMono-Regular.ttf");
pub const PLEX_MEDIUM: &[u8] = include_bytes!("../../../../shared/fonts/IBMPlexMono-Medium.ttf");

// IMPORTANT (iced 0.13 font matching): iced registers embedded fonts in its
// cosmic-text fontdb, which keys faces by the **typographic family** from the
// name table (name_id 16, falling back to name_id 1). All four bundled TTFs
// set name_id 16 to "Chakra Petch" / "IBM Plex Mono" — the weight lives in
// name_id 17 (SemiBold / Medium / Regular), NOT in the family string. A
// `Font::with_name("Chakra Petch SemiBold")` therefore matches nothing and
// iced silently falls back to the default system font (DejaVu), which is how
// the menu "≡" rendered as a tofu box. Match family + weight separately:

/// Display/labels: Chakra Petch SemiBold (headers/lamp).
pub const FONT_HEADER: Font = Font {
    family: iced::font::Family::Name("Chakra Petch"),
    weight: iced::font::Weight::Semibold,
    ..Font::DEFAULT
};
/// Stage labels, chips: Chakra Petch Medium.
pub const FONT_LABEL: Font = Font {
    family: iced::font::Family::Name("Chakra Petch"),
    weight: iced::font::Weight::Medium,
    ..Font::DEFAULT
};
/// Data/numbers: IBM Plex Mono Regular.
pub const FONT_MONO: Font = Font {
    family: iced::font::Family::Name("IBM Plex Mono"),
    ..Font::DEFAULT
};
/// Data/numbers, emphasized: IBM Plex Mono Medium.
pub const FONT_MONO_MEDIUM: Font = Font {
    family: iced::font::Family::Name("IBM Plex Mono"),
    weight: iced::font::Weight::Medium,
    ..Font::DEFAULT
};

// ---------------------------------------------------------------------------
// Component styles
// ---------------------------------------------------------------------------

/// Stage card: panel background (subtle vertical shading), hairline border,
/// 2 px radius max.
pub fn stage_card(theme: &iced::Theme) -> container::Style {
    let _ = theme;
    container::Style {
        background: Some(panel_background()),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: LINE,
        },
        ..container::Style::default()
    }
}

/// Flat transparent button (menu button, chips).
pub fn flat_button(theme: &iced::Theme, _status: button::Status) -> button::Style {
    let _ = theme;
    button::Style {
        background: Some(iced::Background::Color(Color::TRANSPARENT)),
        border: Border::default(),
        text_color: CREAM,
        ..button::Style::default()
    }
}

/// Engraved look for secondary buttons: no fill, hairline border.
pub fn engraved_button(theme: &iced::Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let (fg, border) = match status {
        button::Status::Hovered | button::Status::Pressed => (CREAM, DIM),
        _ => (DIM, LINE),
    };
    button::Style {
        background: Some(iced::Background::Color(PANEL)),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: border,
        },
        text_color: fg,
        ..button::Style::default()
    }
}

/// The ON AIR toggle: engraved ring in standby, amber glow ring on air.
/// While held (pressed), the cold ring brightens from line to dim — the
/// short 200 ms halo flash in `app.rs` covers the design's ring pulse, and
/// this is the instant held-state feedback. No bounce/spring.
pub fn air_button(theme: &iced::Theme, status: button::Status, live: bool) -> button::Style {
    let _ = theme;
    let (ring, fg) = if live { (AMBER, CREAM) } else { (LINE, DIM) };
    let mut style = button::Style {
        background: Some(iced::Background::Color(INK)),
        border: Border {
            radius: 999.0.into(),
            width: if live { 3.0 } else { 2.0 },
            color: ring,
        },
        text_color: fg,
        ..button::Style::default()
    };
    if matches!(status, button::Status::Pressed) {
        style.border.color = if live { AMBER } else { DIM };
    }
    style
}

/// Text input styled for the settings panel (dark, hairline).
pub fn settings_input(theme: &iced::Theme, _status: text_input::Status) -> text_input::Style {
    let _ = theme;
    text_input::Style {
        background: iced::Background::Color(INK),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: LINE,
        },
        icon: Color::TRANSPARENT,
        placeholder: DIM,
        value: CREAM,
        selection: AMBER,
    }
}

/// Bind-address pick list (settings panel): hairline, warm, same language as
/// the text input next to it. The dropdown menu keeps the theme defaults,
/// which derive from this palette (warm, dark) and therefore stay on-token.
pub fn settings_pick_list(
    theme: &iced::Theme,
    status: pick_list::Status,
) -> pick_list::Style {
    let _ = theme;
    let border = match status {
        pick_list::Status::Hovered | pick_list::Status::Opened => DIM,
        _ => LINE,
    };
    pick_list::Style {
        text_color: CREAM,
        placeholder_color: DIM,
        handle_color: AMBER,
        background: iced::Background::Color(INK),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: border,
        },
    }
}

// ---------------------------------------------------------------------------
// Typography notes (binding, design.md §Typography)
// ---------------------------------------------------------------------------

// NOTE (letter-spacing / "+8% tracking"): design.md calls for wide tracking
// (+8%) on the all-caps Chakra Petch stage labels. iced 0.13's text pipeline
// (cosmic-text) exposes NO letter-spacing on the `Text` widget — there is no
// `tracking`/`letter_spacing` field anywhere in `iced_core::widget::text` or
// `iced_widget::text` (verified against iced 0.13.1 / iced-widget 0.13.4).
// Emulating tracking by inserting space glyphs between characters would be
// ~+50% tracking (a full space at 10 px is far wider than +8%) and would
// corrupt the strings the glyph-safety test audits, so we do NOT fake it.
// Instead the "wide, engraved, silkscreen" reading comes from: all-caps
// labels, Chakra Petch's squared letterforms, DIM color, and the generous
// 4 px/8 px gaps around each label. Documented here so the gap between the
// design brief and the widget toolkit is explicit rather than silent.
//
