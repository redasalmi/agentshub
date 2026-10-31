//! The app's fonts, its light and dark themes, and the colors the theme files
//! have no place for: the simple-base roles the app draws with itself, and the
//! terminals' ANSI palette.

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeSet};
use gpui_kit::*;
use libghostty_vt::style::RgbColor;

use crate::settings::{Appearance, Settings};
use crate::terminal::Colors;
use crate::terminal_view::rgb_color;

const THEMES: &str = include_str!("../assets/themes.json");

const FONTS: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-Regular.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-Medium.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-SemiBold.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Regular.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Medium.ttf"),
    include_bytes!("../assets/fonts/YoungSerif-Regular.ttf"),
];

/// The display face, for titles.
pub const SERIF: &str = "Young Serif";

const fn hex(rgb: u32) -> RgbColor {
    RgbColor {
        r: (rgb >> 16) as u8,
        g: (rgb >> 8) as u8,
        b: rgb as u8,
    }
}

/// Black, red, green, yellow, blue, magenta, cyan and white, then their bright
/// variants. On the light background, white is a grey that stays readable.
const LIGHT_ANSI: [RgbColor; 16] = [
    hex(0x23211C),
    hex(0xA5301D),
    hex(0x2B6A44),
    hex(0x85580A),
    hex(0x2A547D),
    hex(0x8A3F7A),
    hex(0x1F6A6E),
    hex(0x8E8676),
    hex(0x6B655A),
    hex(0xB8402B),
    hex(0x2F7A4D),
    hex(0xB07A12),
    hex(0x3A6C9C),
    hex(0x9C4F8C),
    hex(0x2B7F84),
    hex(0xC9C0AE),
];

pub const DARK_ANSI: [RgbColor; 16] = [
    hex(0x4A463D),
    hex(0xF28C79),
    hex(0x8FD0A6),
    hex(0xE9BD62),
    hex(0x8FB8E0),
    hex(0xD3A0C8),
    hex(0x8CCFCB),
    hex(0xC6BFB0),
    hex(0x6E6A5E),
    hex(0xF7A898),
    hex(0xABDEBD),
    hex(0xF0CE85),
    hex(0xABCBEA),
    hex(0xE0B9D7),
    hex(0xA9DDDA),
    hex(0xF0EBE0),
];

/// Loads the fonts and themes. `follow_system` then puts the theme in the
/// mode the settings ask for, once they're loaded.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect())
        .expect("bundled fonts are valid");
    let themes: ThemeSet = serde_json::from_str(THEMES).expect("bundled themes are valid");
    let theme = Theme::global_mut(cx);
    for config in themes.themes {
        if config.mode.is_dark() {
            theme.dark_theme = Rc::new(config);
        } else {
            theme.light_theme = Rc::new(config);
        }
    }
}

/// The system appearance the theme last followed.
struct SystemAppearance(WindowAppearance);

impl Global for SystemAppearance {}

/// Puts the theme in the mode the settings ask for, if the system changed
/// mode since the theme last followed it. A window opened again from the tray
/// calls this, as it misses the changes made while it was closed.
pub fn follow_system(window: &mut Window, cx: &mut App) {
    let appearance = window.appearance();
    if cx
        .try_global::<SystemAppearance>()
        .is_some_and(|last| last.0 == appearance)
    {
        return;
    }
    cx.set_global(SystemAppearance(appearance));
    apply(window, cx);
}

/// Puts the theme in the mode the settings ask for.
pub fn apply(window: &mut Window, cx: &mut App) {
    match cx.global::<Settings>().appearance {
        Appearance::System => Theme::sync_system_appearance(Some(window), cx),
        Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
}

/// The mode the theme isn't in, which the sidebar's button switches to.
pub fn other(cx: &App) -> Appearance {
    if cx.theme().is_dark() {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

pub fn terminal_colors(cx: &App) -> Colors {
    let theme = cx.theme();
    Colors {
        background: rgb_color(theme.background),
        foreground: rgb_color(theme.foreground),
        ansi: if theme.is_dark() {
            DARK_ANSI
        } else {
            LIGHT_ANSI
        },
    }
}

/// A status color: `base` for dots and glyphs, `text` for labels on the
/// app's backgrounds, and `soft`, the tint behind them.
#[derive(Clone, Copy)]
pub struct Tint {
    pub base: Hsla,
    pub text: Hsla,
    pub soft: Hsla,
}

/// The simple-base roles the theme files have no place for.
pub struct Palette {
    /// The fill one step further than `muted`: tracks, keys, pressed rows.
    pub strong: Hsla,
    pub secondary_foreground: Hsla,
    /// Boundaries the user needs to see to find a control.
    pub border_strong: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub on_accent: Hsla,
    pub info: Tint,
    pub warning: Tint,
    pub success: Tint,
    pub danger: Tint,
}

fn color(rgb: u32) -> Hsla {
    gpui_kit::rgb(rgb).into()
}

fn tint(base: u32, text: u32, soft: u32) -> Tint {
    Tint {
        base: color(base),
        text: color(text),
        soft: color(soft),
    }
}

static LIGHT: LazyLock<Palette> = LazyLock::new(|| Palette {
    strong: color(0xEDE6D8),
    secondary_foreground: color(0x555047),
    border_strong: color(0x8E8676),
    accent: color(0x1F4D3A),
    accent_soft: color(0xDDE9E0),
    on_accent: color(0xFDFBF6),
    info: tint(0x3A6C9C, 0x2A547D, 0xDDE6EF),
    warning: tint(0xB07A12, 0x85580A, 0xF5E6C6),
    success: tint(0x2F7A4D, 0x2B6A44, 0xDCEBE0),
    danger: tint(0xB8402B, 0xA5301D, 0xF7DCD4),
});

static DARK: LazyLock<Palette> = LazyLock::new(|| Palette {
    strong: color(0x332F28),
    secondary_foreground: color(0xC6BFB0),
    border_strong: color(0x6E6A5E),
    accent: color(0x93D1B0),
    accent_soft: color(0x1F3A2D),
    on_accent: color(0x12110E),
    info: tint(0x8FB8E0, 0x8FB8E0, 0x1D2C3B),
    warning: tint(0xE9BD62, 0xE9BD62, 0x3D3016),
    success: tint(0x8FD0A6, 0x8FD0A6, 0x1E3526),
    danger: tint(0xF28C79, 0xF28C79, 0x45221B),
});

/// The palette of the mode the theme is in.
pub fn palette(cx: &App) -> &'static Palette {
    if cx.theme().is_dark() { &DARK } else { &LIGHT }
}
