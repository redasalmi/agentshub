//! The app's fonts, its light and dark themes, and the colors the theme files
//! have no place for: one per agent, and the terminals' ANSI palette.

use std::borrow::Cow;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeSet};
use gpui_kit::*;
use libghostty_vt::style::RgbColor;

use crate::agent::Agent;
use crate::terminal::Colors;
use crate::terminal_view::rgb_color;

const THEMES: &str = include_str!("../assets/themes.json");

const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/MonaSans-Regular.ttf"),
    include_bytes!("../assets/fonts/MonaSans-Medium.ttf"),
    include_bytes!("../assets/fonts/MonaSans-SemiBold.ttf"),
];

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
    hex(0x18211C),
    hex(0xB3261E),
    hex(0x2F7A3B),
    hex(0x8A6100),
    hex(0x1F64B0),
    hex(0x9A3C8E),
    hex(0x0E7380),
    hex(0x8A958E),
    hex(0x5E6B63),
    hex(0xCC3B30),
    hex(0x3B8F48),
    hex(0xA07400),
    hex(0x2F78C8),
    hex(0xB04FA4),
    hex(0x158896),
    hex(0xB7C1BA),
];

pub const DARK_ANSI: [RgbColor; 16] = [
    hex(0x4A5751),
    hex(0xE5736A),
    hex(0x8CC98B),
    hex(0xE8C06A),
    hex(0x6FA8E8),
    hex(0xC792D8),
    hex(0x6CC4C8),
    hex(0xC8D1CB),
    hex(0x6E7D74),
    hex(0xF0918A),
    hex(0xA8DCA6),
    hex(0xF2D58E),
    hex(0x94C0F0),
    hex(0xD9AEE6),
    hex(0x93D8DA),
    hex(0xF1F5F2),
];

/// Loads the fonts and themes, in the mode the system is in.
pub fn init(window: &mut Window, cx: &mut App) {
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
    follow_system(window, cx);
}

/// The system appearance the theme last followed.
struct SystemAppearance(WindowAppearance);

impl Global for SystemAppearance {}

/// Puts the theme in the system's mode if the system changed mode since the
/// theme last followed it. A window opened again from the tray calls this, as
/// it misses the changes made while it was closed; the mode the user picked
/// stays when the system didn't change.
pub fn follow_system(window: &mut Window, cx: &mut App) {
    let appearance = window.appearance();
    if cx
        .try_global::<SystemAppearance>()
        .is_some_and(|last| last.0 == appearance)
    {
        return;
    }
    cx.set_global(SystemAppearance(appearance));
    Theme::sync_system_appearance(Some(window), cx);
}

pub fn toggle(window: &mut Window, cx: &mut App) {
    let mode = if cx.theme().is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, Some(window), cx);
}

/// The color that marks an agent's sessions.
pub fn agent_color(agent: Agent, cx: &App) -> Hsla {
    let dark = cx.theme().is_dark();
    let color = match (agent, dark) {
        (Agent::Claude, false) => 0xA8690A,
        (Agent::Claude, true) => 0xE8A53C,
        (Agent::Codex, false) => 0x1F6FB0,
        (Agent::Codex, true) => 0x5DAAE8,
        (Agent::Pi, false) => 0xB23A66,
        (Agent::Pi, true) => 0xEB789F,
    };
    rgb(color).into()
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
