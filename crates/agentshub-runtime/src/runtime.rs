//! The runtime's state, and the intents that change it.

use std::fs;

use agentshub_app::bootstrap::Opened;
use agentshub_app::catalog::Catalog;
use agentshub_app::settings::Settings;
use agentshub_core::lifecycle::Lifecycle;
use agentshub_core::store::{Change, Imported, Project, Store};
use gpui_kit::{Context, EventEmitter, SharedString};

use crate::{Intent, RuntimeEvent};

/// What runs behind the window, and outlives it: the database, the projects
/// and chats as shown, and the chats on their way in or out.
pub struct Runtime {
    store: Store,
    /// The projects and chats, including the changes not saved yet.
    catalog: Catalog,
    /// The chats on their way in or out, the ones being deleted, and which
    /// of them may take the view.
    lifecycle: Lifecycle,
    /// The lock that keeps a second instance off the database.
    _lock: Option<fs::File>,
    /// Why the database can't be used: it didn't open, or another Agents Hub
    /// holds the lock.
    broken: Option<SharedString>,
    /// The settings startup read and the import it finished, until the
    /// window takes them.
    startup: Option<(Settings, Option<Imported>)>,
}

impl EventEmitter<RuntimeEvent> for Runtime {}

impl Runtime {
    pub fn new(opened: Opened, _cx: &mut Context<Self>) -> Self {
        let Opened {
            store,
            lock,
            imported,
            settings,
            projects,
            broken,
        } = opened;
        Self {
            store,
            catalog: Catalog::new(projects),
            lifecycle: Lifecycle::default(),
            _lock: lock,
            broken: broken.map(SharedString::from),
            startup: Some((settings, imported)),
        }
    }

    /// The settings startup read and the database import it finished, if
    /// any, for the window to apply and tell the user about. Only the first
    /// call gets them.
    pub fn take_startup(&mut self) -> (Settings, Option<Imported>) {
        self.startup.take().unwrap_or_default()
    }

    /// The projects and chats, including the changes not saved yet.
    pub fn projects(&self) -> &[Project] {
        self.catalog.projects()
    }

    /// Why the database can't be used, if it can't.
    pub fn broken(&self) -> Option<&SharedString> {
        self.broken.as_ref()
    }

    pub fn apply(&mut self, intent: Intent, cx: &mut Context<Self>) {
        match intent {
            Intent::Change(change) => self.change(change, cx),
        }
    }

    /// Shows `change` at once, and saves it. The projects are listed as saved
    /// once every change shown is, which takes back one that couldn't be.
    fn change(&mut self, change: Change, cx: &mut Context<Self>) {
        let saved = apply_change(&mut self.catalog, &self.store, change);
        cx.emit(RuntimeEvent::CatalogChanged);
        cx.spawn(async move |this, cx| {
            let saved = saved.await;
            this.update(cx, |this, cx| {
                this.catalog.saved();
                if let Err(err) = saved {
                    eprintln!("can't save a change: {err:#}");
                    cx.emit(RuntimeEvent::Notice(format!(
                        "Couldn't save the change, so it is undone: {err}"
                    )));
                }
                cx.emit(RuntimeEvent::CatalogChanged);
            })
            .ok();
        })
        .detach();
    }

    #[doc(hidden)]
    pub fn store(&self) -> &Store {
        &self.store
    }

    #[doc(hidden)]
    pub fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    #[doc(hidden)]
    pub fn lifecycle_mut(&mut self) -> &mut Lifecycle {
        &mut self.lifecycle
    }

    #[doc(hidden)]
    pub fn catalog_mut(&mut self) -> &mut Catalog {
        &mut self.catalog
    }
}

/// Shows `change` in `catalog` and asks `store` to save it, which the future
/// it returns waits for.
pub(crate) fn apply_change(
    catalog: &mut Catalog,
    store: &Store,
    change: Change,
) -> impl Future<Output = anyhow::Result<()>> + use<> {
    catalog.change(&change);
    store.change(change)
}
