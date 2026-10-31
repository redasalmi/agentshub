//! The app's fonts, its light and dark themes, and the colors the theme files
//! have no place for: the extra palette roles the app draws with itself, and the
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
    hex(0x0F1011),
    hex(0xC93B3B),
    hex(0x1E7F35),
    hex(0x8A6200),
    hex(0x4F52D6),
    hex(0x7C3AED),
    hex(0x007A88),
    hex(0x8A8F98),
    hex(0x62666D),
    hex(0xEB5757),
    hex(0x27A644),
    hex(0xB88600),
    hex(0x6366F1),
    hex(0x8B5CF6),
    hex(0x02A3B5),
    hex(0xC8CCD2),
];

pub const DARK_ANSI: [RgbColor; 16] = [
    hex(0x23252A),
    hex(0xEB5757),
    hex(0x3FBF5E),
    hex(0xF2C94C),
    hex(0x818CF8),
    hex(0xA78BFA),
    hex(0x02B8CC),
    hex(0xD0D6E0),
    hex(0x62666D),
    hex(0xF27A7A),
    hex(0x5FD17A),
    hex(0xF7D978),
    hex(0xA5B4FC),
    hex(0xC4B5FD),
    hex(0x4FD6E6),
    hex(0xF7F8F8),
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

/// The palette roles the theme files have no place for.
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
    strong: color(0xEDEEF0),
    secondary_foreground: color(0x3C4049),
    border_strong: color(0x8A8F98),
    accent: color(0x5F6600),
    accent_soft: color(0xF6F9C6),
    on_accent: color(0xFFFFFF),
    info: tint(0x02B8CC, 0x007A88, 0xDDF4F7),
    warning: tint(0xE0A800, 0x8A6200, 0xFBF1D4),
    success: tint(0x27A644, 0x1E7F35, 0xE1F4E5),
    danger: tint(0xEB5757, 0xC93B3B, 0xFCE6E6),
});

static DARK: LazyLock<Palette> = LazyLock::new(|| Palette {
    strong: color(0x23252A),
    secondary_foreground: color(0xD0D6E0),
    border_strong: color(0x62666D),
    accent: color(0xE4F222),
    accent_soft: color(0x2D3013),
    on_accent: color(0x08090A),
    info: tint(0x02B8CC, 0x02B8CC, 0x0E282B),
    warning: tint(0xF2C94C, 0xF2C94C, 0x2F2A19),
    success: tint(0x27A644, 0x3FBF5E, 0x122518),
    danger: tint(0xEB5757, 0xEB5757, 0x2E1A1B),
});

/// The palette of the mode the theme is in.
pub fn palette(cx: &App) -> &'static Palette {
    if cx.theme().is_dark() { &DARK } else { &LIGHT }
}
