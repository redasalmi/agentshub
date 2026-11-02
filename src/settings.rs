//! The app's settings, kept in the database and shared with the app as a
//! global.

use gpui_kit::*;

use crate::db::Db;

const CLOSE_TO_TRAY: &str = "close_to_tray";
const APPEARANCE: &str = "appearance";
const THEME: &str = "theme";
const NOTIFICATION_POPUPS: &str = "notification_popups";
const NOTIFICATION_SOUND: &str = "notification_sound";
const SIDEBAR_WIDTH: &str = "sidebar_width";
const WORKTREE_BY_DEFAULT: &str = "worktree_by_default";

/// The sidebar's width until the user drags its edge.
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 340.;

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
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// The saved settings. One that can't be read has its default.
    pub fn load(db: &Db) -> Self {
        fn read<T>(value: rusqlite::Result<Option<T>>) -> Option<T> {
            value
                .inspect_err(|err| eprintln!("can't read the settings: {err}"))
                .ok()
                .flatten()
        }
        let defaults = Self::default();
        Self {
            close_to_tray: read(db.flag(CLOSE_TO_TRAY)).unwrap_or(defaults.close_to_tray),
            appearance: read(db.text(APPEARANCE))
                .and_then(|key| Appearance::from_key(&key))
                .unwrap_or(defaults.appearance),
            theme: read(db.text(THEME))
                .and_then(|key| ThemeFamily::from_key(&key))
                .unwrap_or(defaults.theme),
            notification_popups: read(db.flag(NOTIFICATION_POPUPS))
                .unwrap_or(defaults.notification_popups),
            notification_sound: read(db.flag(NOTIFICATION_SOUND))
                .unwrap_or(defaults.notification_sound),
            sidebar_width: read(db.number(SIDEBAR_WIDTH))
                .map(|width| width as f32)
                .filter(|width| width.is_finite() && *width > 0.)
                .unwrap_or(defaults.sidebar_width),
            worktree_by_default: read(db.flag(WORKTREE_BY_DEFAULT))
                .unwrap_or(defaults.worktree_by_default),
        }
    }

    pub fn set_notification_popups(db: &Db, value: bool, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            notification_popups: value,
            ..*cx.global::<Self>()
        });
        db.set_flag(NOTIFICATION_POPUPS, value)
    }

    pub fn set_notification_sound(db: &Db, value: bool, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            notification_sound: value,
            ..*cx.global::<Self>()
        });
        db.set_flag(NOTIFICATION_SOUND, value)
    }

    // The setters apply the setting, which only lasts until the app quits
    // when it can't be saved.

    pub fn set_close_to_tray(db: &Db, value: bool, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            close_to_tray: value,
            ..*cx.global::<Self>()
        });
        db.set_flag(CLOSE_TO_TRAY, value)
    }

    pub fn set_worktree_by_default(db: &Db, value: bool, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            worktree_by_default: value,
            ..*cx.global::<Self>()
        });
        db.set_flag(WORKTREE_BY_DEFAULT, value)
    }

    pub fn set_sidebar_width(db: &Db, value: f32, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            sidebar_width: value,
            ..*cx.global::<Self>()
        });
        db.set_number(SIDEBAR_WIDTH, value.into())
    }

    /// Callers put the theme in the new mode with `appearance::apply`.
    pub fn set_appearance(db: &Db, value: Appearance, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            appearance: value,
            ..*cx.global::<Self>()
        });
        db.set_text(APPEARANCE, value.key())
    }

    /// Callers put the theme in the new family with `appearance::apply`.
    pub fn set_theme(db: &Db, value: ThemeFamily, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            theme: value,
            ..*cx.global::<Self>()
        });
        db.set_text(THEME, value.key())
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, ThemeFamily};

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
}
