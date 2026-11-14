//! The app's settings, kept in the database and shared with the app as a
//! global.

use std::collections::HashMap;

use gpui_kit::*;
use rusqlite::types::{FromSql, Value};

use crate::store::Store;

const CLOSE_TO_TRAY: &str = "close_to_tray";
const APPEARANCE: &str = "appearance";
const THEME: &str = "theme";
const NOTIFICATION_POPUPS: &str = "notification_popups";
const NOTIFICATION_SOUND: &str = "notification_sound";
const SIDEBAR_WIDTH: &str = "sidebar_width";
const WORKTREE_BY_DEFAULT: &str = "worktree_by_default";
const PROJECT_NAMES: &str = "project_names";
const RAIL_WIDTH: &str = "rail_width";
const SHELL_SHARE: &str = "shell_share";
const MOTION: &str = "motion";
const TERMINAL_FONT_SIZE: &str = "terminal_font_size";

/// The sidebar's width until the user drags its edge.
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 340.;

/// The rail's width while it names the projects, until the user drags its
/// edge.
pub const DEFAULT_RAIL_WIDTH: f32 = 160.;

/// How much of the window's height a project's shell takes under its chat,
/// until the user drags its edge.
pub const DEFAULT_SHELL_SHARE: f32 = 0.35;

/// The terminals' text size until the user zooms them, and how far they zoom.
pub const DEFAULT_TERMINAL_FONT_SIZE: f32 = 14.;
pub const MIN_TERMINAL_FONT_SIZE: f32 = 9.;
pub const MAX_TERMINAL_FONT_SIZE: f32 = 28.;

/// The mode the theme is in.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Appearance {
    /// Whichever mode the system is in, changing when it does.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// How the database spells it.
    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|appearance| appearance.key() == key)
    }
}

/// The family of themes the app draws with, each with a light and a dark
/// theme.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum ThemeFamily {
    #[default]
    Linear,
    Primer,
}

impl ThemeFamily {
    pub const ALL: [Self; 2] = [Self::Linear, Self::Primer];

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Primer => "Primer",
        }
    }

    /// How the database spells it.
    fn key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Primer => "primer",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|family| family.key() == key)
    }
}

/// How much the app animates.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Motion {
    /// As the desktop's reduced-motion preference says, changing when it does.
    #[default]
    System,
    Full,
    Reduced,
}

impl Motion {
    pub const ALL: [Self; 3] = [Self::System, Self::Full, Self::Reduced];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Full => "Full",
            Self::Reduced => "Reduced",
        }
    }

    /// How the database spells it.
    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Full => "full",
            Self::Reduced => "reduced",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|motion| motion.key() == key)
    }
}

#[derive(Clone, Copy)]
pub struct Settings {
    /// Whether closing the window leaves the app running in the tray.
    pub close_to_tray: bool,
    pub appearance: Appearance,
    pub theme: ThemeFamily,
    /// Whether a desktop notification says when an agent finishes or needs
    /// the user.
    pub notification_popups: bool,
    /// Whether a sound plays then, with or without the notification.
    pub notification_sound: bool,
    /// How wide the user last dragged the sidebar, in logical pixels.
    pub sidebar_width: f32,
    /// Whether a new chat in a git project starts in a worktree of its own
    /// unless the user unticks it.
    pub worktree_by_default: bool,
    /// Whether the sidebar's rail names its projects, or shows only their
    /// marks.
    pub project_names: bool,
    /// How wide the user last dragged the rail while it names the projects,
    /// in logical pixels.
    pub rail_width: f32,
    /// How much of the window's height the user last dragged the projects'
    /// shell to, the same for every project.
    pub shell_share: f32,
    pub motion: Motion,
    pub terminal_font_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_to_tray: false,
            appearance: Appearance::default(),
            theme: ThemeFamily::default(),
            notification_popups: true,
            notification_sound: true,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            worktree_by_default: false,
            project_names: true,
            rail_width: DEFAULT_RAIL_WIDTH,
            shell_share: DEFAULT_SHELL_SHARE,
            motion: Motion::default(),
            terminal_font_size: DEFAULT_TERMINAL_FONT_SIZE,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// The settings saved in `saved`, by name. One that can't be read has its
    /// default.
    pub fn load(saved: &HashMap<String, Value>) -> Self {
        fn read<T: FromSql>(saved: &HashMap<String, Value>, key: &str) -> Option<T> {
            T::column_result(saved.get(key)?.into())
                .inspect_err(|err| eprintln!("can't read the settings: {err}"))
                .ok()
        }
        let defaults = Self::default();
        Self {
            close_to_tray: read::<bool>(saved, CLOSE_TO_TRAY).unwrap_or(defaults.close_to_tray),
            appearance: read::<String>(saved, APPEARANCE)
                .and_then(|key| Appearance::from_key(&key))
                .unwrap_or(defaults.appearance),
            theme: read::<String>(saved, THEME)
                .and_then(|key| ThemeFamily::from_key(&key))
                .unwrap_or(defaults.theme),
            notification_popups: read::<bool>(saved, NOTIFICATION_POPUPS)
                .unwrap_or(defaults.notification_popups),
            notification_sound: read::<bool>(saved, NOTIFICATION_SOUND)
                .unwrap_or(defaults.notification_sound),
            sidebar_width: read::<f64>(saved, SIDEBAR_WIDTH)
                .map(|width| width as f32)
                .filter(|width| width.is_finite() && *width > 0.)
                .unwrap_or(defaults.sidebar_width),
            worktree_by_default: read::<bool>(saved, WORKTREE_BY_DEFAULT)
                .unwrap_or(defaults.worktree_by_default),
            project_names: read::<bool>(saved, PROJECT_NAMES).unwrap_or(defaults.project_names),
            rail_width: read::<f64>(saved, RAIL_WIDTH)
                .map(|width| width as f32)
                .filter(|width| width.is_finite() && *width > 0.)
                .unwrap_or(defaults.rail_width),
            shell_share: read::<f64>(saved, SHELL_SHARE)
                .map(|share| share as f32)
                .filter(|share| share.is_finite() && *share > 0. && *share < 1.)
                .unwrap_or(defaults.shell_share),
            motion: read::<String>(saved, MOTION)
                .and_then(|key| Motion::from_key(&key))
                .unwrap_or(defaults.motion),
            terminal_font_size: read::<f64>(saved, TERMINAL_FONT_SIZE)
                .map(|size| size as f32)
                .filter(|size| (MIN_TERMINAL_FONT_SIZE..=MAX_TERMINAL_FONT_SIZE).contains(size))
                .unwrap_or(defaults.terminal_font_size),
        }
    }

    pub fn set_notification_popups(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            notification_popups: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(NOTIFICATION_POPUPS, value)
    }

    pub fn set_notification_sound(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            notification_sound: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(NOTIFICATION_SOUND, value)
    }

    // The setters apply the setting, which only lasts until the app quits
    // when it can't be saved.

    pub fn set_close_to_tray(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            close_to_tray: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(CLOSE_TO_TRAY, value)
    }

    pub fn set_worktree_by_default(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            worktree_by_default: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(WORKTREE_BY_DEFAULT, value)
    }

    pub fn set_project_names(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            project_names: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(PROJECT_NAMES, value)
    }

    pub fn set_rail_width(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            rail_width: value,
            ..*cx.global::<Self>()
        });
        store.set_number(RAIL_WIDTH, value.into())
    }

    pub fn set_shell_share(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            shell_share: value,
            ..*cx.global::<Self>()
        });
        store.set_number(SHELL_SHARE, value.into())
    }

    pub fn set_sidebar_width(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            sidebar_width: value,
            ..*cx.global::<Self>()
        });
        store.set_number(SIDEBAR_WIDTH, value.into())
    }

    /// Callers put the theme in the new mode with `appearance::apply`.
    pub fn set_appearance(
        store: &Store,
        value: Appearance,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            appearance: value,
            ..*cx.global::<Self>()
        });
        store.set_text(APPEARANCE, value.key())
    }

    /// Callers make it take effect with `appearance::apply_motion`.
    pub fn set_motion(
        store: &Store,
        value: Motion,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            motion: value,
            ..*cx.global::<Self>()
        });
        store.set_text(MOTION, value.key())
    }

    /// Kept within the sizes the terminals zoom to.
    pub fn set_terminal_font_size(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        let value = value.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE);
        cx.set_global(Self {
            terminal_font_size: value,
            ..*cx.global::<Self>()
        });
        store.set_number(TERMINAL_FONT_SIZE, value.into())
    }

    /// Callers put the theme in the new family with `appearance::apply`.
    pub fn set_theme(
        store: &Store,
        value: ThemeFamily,
        cx: &mut App,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        cx.set_global(Self {
            theme: value,
            ..*cx.global::<Self>()
        });
        store.set_text(THEME, value.key())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APPEARANCE, Appearance, CLOSE_TO_TRAY, MOTION, Motion, SHELL_SHARE, SIDEBAR_WIDTH,
        Settings, ThemeFamily,
    };
    use crate::store::Db;

    #[test]
    fn loads_what_was_saved() {
        let db = Db::open_in_memory().unwrap();
        db.set_flag(CLOSE_TO_TRAY, true).unwrap();
        db.set_text(APPEARANCE, Appearance::Dark.key()).unwrap();
        // Saved as a whole number, it reads back as an integer.
        db.set_number(SIDEBAR_WIDTH, 450.).unwrap();
        db.set_number(SHELL_SHARE, 0.5).unwrap();
        db.set_text(MOTION, "wobbly").unwrap();
        let settings = Settings::load(&db.settings().unwrap());
        assert!(settings.close_to_tray);
        assert_eq!(settings.appearance, Appearance::Dark);
        assert_eq!(settings.sidebar_width, 450.);
        assert_eq!(settings.shell_share, 0.5);
        assert_eq!(settings.motion, Settings::default().motion);
    }

    #[test]
    fn appearances_read_back_as_saved() {
        for appearance in Appearance::ALL {
            assert_eq!(Appearance::from_key(appearance.key()), Some(appearance));
        }
        assert_eq!(Appearance::from_key("sepia"), None);
    }

    #[test]
    fn theme_families_read_back_as_saved() {
        for family in ThemeFamily::ALL {
            assert_eq!(ThemeFamily::from_key(family.key()), Some(family));
        }
        assert_eq!(ThemeFamily::from_key("solarized"), None);
    }

    #[test]
    fn motions_read_back_as_saved() {
        for motion in Motion::ALL {
            assert_eq!(Motion::from_key(motion.key()), Some(motion));
        }
        assert_eq!(Motion::from_key("wobbly"), None);
    }
}
