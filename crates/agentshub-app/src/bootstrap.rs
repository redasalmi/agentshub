//! Opening the database and the settings the window starts on.

use std::fs;

use anyhow::Context as _;
use gpui_kit::WindowBounds;

use crate::settings::{self, Settings};
use agentshub_core::store::{Db, Imported, Project, Store, open_locked};

/// The database as startup opened it.
pub struct Opened {
    pub store: Store,
    /// Keeps a second Agents Hub off the database while it is held.
    pub lock: Option<fs::File>,
    pub imported: Option<Imported>,
    pub settings: Settings,
    /// Where a layout restart left the window, for it to open there.
    pub restart_bounds: Option<WindowBounds>,
    pub projects: Vec<Project>,
    /// Why the database can't be used, in which case an empty one in memory
    /// stands in for it.
    pub broken: Option<String>,
}

/// Opens the database where it lives, with the settings saved in it. Taking
/// the lock, finishing an import and migrating, which first backs the
/// database up, can each take a while, so they run on a thread of their own.
pub async fn open() -> Opened {
    let (store, lock, imported, broken) = smol::unblock(|| {
        let opened = open_locked().and_then(|(db, lock, imported)| {
            let store = Store::new(db).context("Couldn't read the database")?;
            Ok((store, lock, imported))
        });
        match opened {
            Ok((store, lock, imported)) => (store, Some(lock), imported, None),
            Err(err) => {
                eprintln!("{err:#}");
                let db = Db::open_in_memory().expect("SQLite opens in memory");
                let store = Store::new(db).expect("the database's threads start");
                (store, None, None, Some(format!("{err:#}")))
            }
        }
    })
    .await;
    let (settings, restart_bounds) = store
        .settings()
        .await
        .map(|saved| (Settings::load(&saved), settings::restart_bounds(&saved)))
        .inspect_err(|err| eprintln!("can't read the settings: {err:#}"))
        .unwrap_or_default();
    let projects = store
        .projects()
        .await
        .inspect_err(|err| eprintln!("can't list the projects: {err:#}"))
        .unwrap_or_default();
    Opened {
        store,
        lock,
        imported,
        settings,
        restart_bounds,
        projects,
        broken,
    }
}
