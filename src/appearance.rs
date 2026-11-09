//! The app's fonts, its families of themes, each with a light and a dark
//! theme, and the colors the theme files have no place for: the extra palette
//! roles the app draws with itself, and the terminals' ANSI palette.

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode, ThemeSet};
use gpui_kit::*;
use libghostty_vt::style::RgbColor;

use crate::settings::{Appearance, Settings, ThemeFamily};
use crate::terminal::Colors;
use crate::terminal_view::rgb_color;

const LINEAR: &str = include_str!("../assets/themes/linear.json");
const PRIMER: &str = include_str!("../assets/themes/primer.json");

const FONTS: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/InterDisplay-Medium.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
];

/// The display face, for titles.
pub const DISPLAY: &str = "Inter Display";

const fn hex(rgb: u32) -> RgbColor {
    RgbColor {
        r: (rgb >> 16) as u8,
        g: (rgb >> 8) as u8,
        b: rgb as u8,
    }
}

/// Loads the fonts and the theme files, and puts the default family's themes
/// in place. `follow_system` then puts the theme in the family and mode the
/// settings ask for, once they're loaded.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect())
        .expect("bundled fonts are valid");
    let themes = Themes {
        linear: Pair::parse(LINEAR),
        primer: Pair::parse(PRIMER),
    };
    let pair = themes.of(ThemeFamily::default());
    let theme = Theme::global_mut(cx);
    theme.light_theme = pair.light.clone();
    theme.dark_theme = pair.dark.clone();
    cx.set_global(themes);
}

/// Each family's theme files.
struct Themes {
    linear: Pair,
    primer: Pair,
}

impl Global for Themes {}

impl Themes {
    fn of(&self, family: ThemeFamily) -> &Pair {
        match family {
            ThemeFamily::Linear => &self.linear,
            ThemeFamily::Primer => &self.primer,
        }
    }
}

/// A family's light and dark themes.
struct Pair {
    light: Rc<ThemeConfig>,
    dark: Rc<ThemeConfig>,
}

impl Pair {
    fn parse(file: &str) -> Self {
        let themes: ThemeSet = serde_json::from_str(file).expect("bundled themes are valid");
        let (mut light, mut dark) = (None, None);
        for config in themes.themes {
            let mode = if config.mode.is_dark() {
                &mut dark
            } else {
                &mut light
            };
            *mode = Some(Rc::new(config));
        }
        Self {
            light: light.expect("each family has a light theme"),
            dark: dark.expect("each family has a dark theme"),
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

/// Puts the theme in the family and mode the settings ask for.
pub fn apply(window: &mut Window, cx: &mut App) {
    let settings = *cx.global::<Settings>();
    let pair = cx.global::<Themes>().of(settings.theme);
    let (light, dark) = (pair.light.clone(), pair.dark.clone());
    // `Theme::change` loads the mode's theme even when the mode stays, so
    // the family's themes take effect either way.
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    match settings.appearance {
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
        ansi: scheme(cx).ansi,
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

/// What one theme draws with beyond its theme file.
struct Scheme {
    palette: Palette,
    /// Black, red, green, yellow, blue, magenta, cyan and white, then their
    /// bright variants.
    ansi: [RgbColor; 16],
}

static LINEAR_LIGHT: LazyLock<Scheme> = LazyLock::new(|| Scheme {
    palette: Palette {
        strong: color(0xEDEEF0),
        secondary_foreground: color(0x3C4049),
        border_strong: color(0x8A8F98),
        accent: color(0x5F6600),
        accent_soft: color(0xF6F9C6),
        info: tint(0x02B8CC, 0x007A88, 0xDDF4F7),
        warning: tint(0xE0A800, 0x8A6200, 0xFBF1D4),
        success: tint(0x27A644, 0x1E7F35, 0xE1F4E5),
        danger: tint(0xEB5757, 0xC93B3B, 0xFCE6E6),
    },
    // On the light background, white is a grey that stays readable.
    ansi: [
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
    ],
});

static LINEAR_DARK: LazyLock<Scheme> = LazyLock::new(|| Scheme {
    palette: Palette {
        strong: color(0x23252A),
        secondary_foreground: color(0xD0D6E0),
        border_strong: color(0x62666D),
        accent: color(0xE4F222),
        accent_soft: color(0x2D3013),
        info: tint(0x02B8CC, 0x02B8CC, 0x0E282B),
        warning: tint(0xF2C94C, 0xF2C94C, 0x2F2A19),
        success: tint(0x27A644, 0x3FBF5E, 0x122518),
        danger: tint(0xEB5757, 0xEB5757, 0x2E1A1B),
    },
    ansi: [
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
    ],
});

// Primer's working color is the terminal cyan, as its accent blue already
// marks the selection.

static PRIMER_LIGHT: LazyLock<Scheme> = LazyLock::new(|| Scheme {
    palette: Palette {
        strong: color(0xE6EAEF),
        secondary_foreground: color(0x3D444D),
        border_strong: color(0x818B98),
        accent: color(0x0969DA),
        accent_soft: color(0xDDF4FF),
        info: tint(0x1B7C83, 0x136A70, 0xDDF6F7),
        warning: tint(0xBF8700, 0x9A6700, 0xFFF8C5),
        success: tint(0x1F883D, 0x1A7F37, 0xDAFBE1),
        danger: tint(0xCF222E, 0xD1242F, 0xFFEBE9),
    },
    // On the light background, white is a grey that stays readable.
    ansi: [
        hex(0x24292F),
        hex(0xCF222E),
        hex(0x116329),
        hex(0x4D2D00),
        hex(0x0969DA),
        hex(0x8250DF),
        hex(0x1B7C83),
        hex(0x6E7781),
        hex(0x57606A),
        hex(0xA40E26),
        hex(0x1A7F37),
        hex(0x633C01),
        hex(0x218BFF),
        hex(0xA475F9),
        hex(0x3192AA),
        hex(0x8C959F),
    ],
});

static PRIMER_DARK: LazyLock<Scheme> = LazyLock::new(|| Scheme {
    palette: Palette {
        strong: color(0x212830),
        secondary_foreground: color(0xB7BDC8),
        border_strong: color(0x656C76),
        accent: color(0x4493F8),
        accent_soft: color(0x13233A),
        info: tint(0x39C5CF, 0x39C5CF, 0x142C33),
        warning: tint(0xD29922, 0xD29922, 0x272215),
        success: tint(0x3FB950, 0x3FB950, 0x12261E),
        danger: tint(0xF85149, 0xF85149, 0x25181C),
    },
    ansi: [
        hex(0x2F3742),
        hex(0xFF7B72),
        hex(0x3FB950),
        hex(0xD29922),
        hex(0x58A6FF),
        hex(0xBE8FFF),
        hex(0x39C5CF),
        hex(0xF0F6FC),
        hex(0x656C76),
        hex(0xFFA198),
        hex(0x56D364),
        hex(0xE3B341),
        hex(0x79C0FF),
        hex(0xD2A8FF),
        hex(0x56D4DD),
        hex(0xFFFFFF),
    ],
});

/// The colors projects are told apart by, as text on its tint, for light
/// themes and for dark ones. Each family uses the same, as they're not its
/// own roles. Each text reads at 5:1 or more on its tint.
const PROJECT_LIGHT: [(u32, u32); 8] = [
    (0x255EAF, 0xD9E6F7),
    (0x197446, 0xD9F7E8),
    (0x732FD3, 0xE6D9F7),
    (0x93501F, 0xF7E6D9),
    (0xAA2467, 0xF7D9E8),
    (0x1B6E7E, 0xD9F2F7),
    (0x3F7419, 0xE6F7D9),
    (0x7A621A, 0xF7F0D9),
];

const PROJECT_DARK: [(u32, u32); 8] = [
    (0x82A7DB, 0x222E3F),
    (0x47CA88, 0x223F30),
    (0xB190DF, 0x2E223F),
    (0xD79E75, 0x3F2E22),
    (0xDD88B3, 0x3F2230),
    (0x5DBDD0, 0x223A3F),
    (0x7BC843, 0x2E3F22),
    (0xCEB055, 0x3F3822),
];

/// The color project `id` is marked with. Projects added one after another
/// get different ones.
pub fn project_tint(id: i64, cx: &App) -> Tint {
    let colors = if cx.theme().is_dark() {
        &PROJECT_DARK
    } else {
        &PROJECT_LIGHT
    };
    let (text, soft) = colors[id.rem_euclid(colors.len() as i64) as usize];
    tint(text, text, soft)
}

/// What the theme in place draws with beyond its theme file.
fn scheme(cx: &App) -> &'static Scheme {
    match (cx.global::<Settings>().theme, cx.theme().is_dark()) {
        (ThemeFamily::Linear, false) => &LINEAR_LIGHT,
        (ThemeFamily::Linear, true) => &LINEAR_DARK,
        (ThemeFamily::Primer, false) => &PRIMER_LIGHT,
        (ThemeFamily::Primer, true) => &PRIMER_DARK,
    }
}

/// The palette of the theme in place.
pub fn palette(cx: &App) -> &'static Palette {
    &scheme(cx).palette
}
