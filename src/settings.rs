//! The app's settings, kept in the database and shared with the app as a
//! global.

use gpui_kit::*;

use crate::db::Db;

const CLOSE_TO_TRAY: &str = "close_to_tray";

#[derive(Clone, Copy, Default)]
pub struct Settings {
    /// Whether closing the window leaves the app running in the tray.
    pub close_to_tray: bool,
}

impl Global for Settings {}

impl Settings {
    pub fn load(db: &Db) -> Self {
        let close_to_tray = db
            .flag(CLOSE_TO_TRAY)
            .inspect_err(|err| eprintln!("can't read the settings: {err}"))
            .ok()
            .flatten()
            .unwrap_or_default();
        Self { close_to_tray }
    }

    /// Applies the setting, which only lasts until the app quits when it
    /// can't be saved.
    pub fn set_close_to_tray(db: &Db, value: bool, cx: &mut App) -> rusqlite::Result<()> {
        cx.set_global(Self {
            close_to_tray: value,
        });
        db.set_flag(CLOSE_TO_TRAY, value)
    }
}
