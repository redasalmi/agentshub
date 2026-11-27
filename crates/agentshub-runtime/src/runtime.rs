//! The runtime's state, and the intents that change it.

use std::fs;

use agentshub_app::bootstrap::Opened;
use agentshub_app::catalog::Catalog;
use agentshub_app::settings::Settings;
use agentshub_core::agent::Agent;
use agentshub_core::lifecycle::Lifecycle;
use agentshub_core::store::{Change, Imported, Project, Store};
use agentshub_core::worktree;
use gpui_kit::{Context, EventEmitter, SharedString, Task};

use crate::chats::ChatRuntime;
use crate::sessions::Following;
use crate::terminal::Colors;
use crate::{Intent, RuntimeEvent};

/// What runs behind the window, and outlives it: the database, the projects
/// and chats as shown, and the chats on their way in or out.
pub struct Runtime {
    pub(crate) store: Store,
    /// The projects and chats, including the changes not saved yet.
    catalog: Catalog,
    /// The chats on their way in or out, the ones being deleted, and which
    /// of them may take the view.
    pub(crate) lifecycle: Lifecycle,
    /// The sessions the CLIs report, and the ones Codex's files show.
    pub(crate) following: Following,
    /// Open chats, including exited terminals and history whose CLI couldn't
    /// start. Running processes keep running while another chat is shown,
    /// until the user closes them.
    pub(crate) chats: Vec<ChatRuntime>,
    /// The chat that is shown.
    pub(crate) active: Option<i64>,
    /// Whether the user can see the shown chat (see `Intent::SetFocused`).
    pub(crate) focused: bool,
    /// Brings the shown chat's turn up to date while it's under way.
    pub(crate) turn_tick: Option<Task<()>>,
    /// What the chats' terminals are painted in, once the window says.
    pub(crate) colors: Option<Colors>,
    /// How many times the window closed (see `Intent::WindowClosed`).
    pub(crate) window_closes: u64,
    /// The worktrees being checked out for new chats. Each is taken by its
    /// chat once git is done, or by shutdown, so none is left without a chat
    /// to remove it.
    pub(crate) checkouts: Vec<worktree::Checkout>,
    /// The agents a project can start, as last looked up off the foreground
    /// thread, and the lookup under way.
    pub(crate) installed: Option<(i64, Vec<Agent>)>,
    pub(crate) installed_lookup: Option<Task<()>>,
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
    pub fn new(opened: Opened, cx: &mut Context<Self>) -> Self {
        let Opened {
            store,
            lock,
            imported,
            settings,
            projects,
            broken,
        } = opened;
        let mut runtime = Self {
            store,
            catalog: Catalog::new(projects),
            lifecycle: Lifecycle::default(),
            following: Following::new(cx),
            chats: Vec::new(),
            active: None,
            focused: false,
            turn_tick: None,
            colors: None,
            window_closes: 0,
            checkouts: Vec::new(),
            installed: None,
            installed_lookup: None,
            _lock: lock,
            broken: broken.map(SharedString::from),
            startup: Some((settings, imported)),
        };
        runtime.update_codex_watch();
        runtime
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

    /// Does what a view asked for.
    pub fn apply(&mut self, intent: Intent, cx: &mut Context<Self>) {
        match intent {
            Intent::Change(change) => self.change(change, cx),
            Intent::OpenChat(id) => self.open_chat(id, cx),
            Intent::RestartChat(id) => self.restart(id, cx),
            Intent::CloseChat(id) => self.close_chat(id, cx),
            Intent::StopChat(id) => {
                self.stop_chat(id, cx);
            }
            Intent::Suspend(id) => self.suspend(id, cx),
            Intent::SetFocused(focused) => self.set_focused(focused, cx),
            Intent::WindowClosed => self.window_closes += 1,
            Intent::NewChat {
                project,
                agent,
                title,
                worktree,
            } => self.start_new_chat(project, agent, title, worktree, cx),
            Intent::StartAgent { project, agent } => self.start_agent(project, agent, cx),
            Intent::LookUpInstalled(project) => self.look_up_installed(project, cx),
            Intent::RemoveProject(id) => self.remove_project(id, cx),
            Intent::ArchiveProject(id) => self.archive_project(id, cx),
            Intent::ArchiveChat(id) => self.archive_chat(id, cx),
            Intent::DeleteChat { chat, worktree } => self.delete_chat(chat, worktree, cx),
            Intent::CloseChats(ids) => self.close_chats(ids, cx),
            Intent::CloseIdle(check) => self.close_idle(check, cx),
            Intent::CloseAll => self.close_all(cx),
        }
    }

    /// Shows `change` at once, and saves it. The projects are listed as saved
    /// once every change shown is, which takes back one that couldn't be.
    pub(crate) fn change(&mut self, change: Change, cx: &mut Context<Self>) {
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
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
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
