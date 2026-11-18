use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::base::{ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{HandleEdge, resize_handle, resize_handle_appearance};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _, Theme,
    ThemeStyled as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::RecursiveMode;

use crate::agent::Agent;
use crate::alerts::{self, Alert};
use crate::appearance;
use crate::history::History;
use crate::palette::{self, Entry, Lead, Pick};
use crate::reports::{self, Reports};
#[cfg(target_os = "linux")]
use crate::scope;
use crate::sessions;
use crate::settings::{
    Appearance, DEFAULT_RAIL_WIDTH, DEFAULT_SHELL_SHARE, DEFAULT_SIDEBAR_WIDTH,
    DEFAULT_TERMINAL_FONT_SIZE, Motion, Settings, ThemeFamily,
};
use crate::sidebar::{self, Held, Sidebar, SidebarEvent, Tone, home_relative};
use crate::status::Status;
use crate::store::{self, Change, Opened, Store};
use crate::terminal::{self, Exited, Pty, StatusChanged, Terminal};
use crate::terminal_view::{self, TerminalView};
use crate::tray;
use crate::worktree::{self, Worktree};

mod actions;
mod chats;
mod codex;
mod commands;
mod dialogs;
mod lifecycle;
mod new_chat;
mod open_chat;
mod preferences;
mod projects;
mod search;
mod session;
mod settings_view;
mod shell;
mod shutdown;
mod status;
mod terminals;

pub use actions::*;

use lifecycle::{Added, Codex, Deleted, Lifecycle, Reaped, Saved, Saving};

use settings_view::render_settings;

/// The narrowest the sidebar can be dragged.
const MIN_SIDEBAR_WIDTH: Pixels = px(300.);

/// The widest the sidebar can be dragged.
const MAX_SIDEBAR_WIDTH: Pixels = px(600.);

/// How much of the window the sidebar always leaves to the chat.
const MIN_MAIN_WIDTH: Pixels = px(360.);

/// The least room a chat's title keeps beside its status and actions; with
/// less, they move below it.
const MIN_TITLE_WIDTH: Pixels = px(200.);
/// The least room beside the sidebar in which the search shows its shortcut
/// as well as what it searches.
const MIN_SEARCH_KEYS_WIDTH: Pixels = px(600.);
/// The least room beside the sidebar in which the footer hints at the search
/// as well as the shortcuts.
const MIN_FOOTER_SEARCH_WIDTH: Pixels = px(560.);
/// How long the shell takes to slide in.
const SHELL_IN: Duration = Duration::from_millis(220);
/// How many of the chats last worked in show while no chat is open.
const RECENT_CHATS: usize = 5;
/// The window's own title bar, which holds the search and the running chats.
const TITLE_BAR_HEIGHT: Pixels = px(48.);

/// The most of the window's height that a chat's earlier sessions take.
const HISTORY_SHARE: f32 = 0.45;

/// The most of the window's height the shell can be dragged to take.
const MAX_SHELL_SHARE: f32 = 0.75;

/// The least height a chat's CLI keeps when the earlier sessions and the
/// shell crowd it. They give up their height to it, in proportion to it.
const MIN_TERMINAL_HEIGHT: Pixels = px(160.);

/// The least height the shell keeps: its header and a few lines.
const MIN_SHELL_HEIGHT: Pixels = px(96.);

/// How often the time the shown chat's prompt has taken so far is brought up
/// to date.
const TURN_INTERVAL: Duration = Duration::from_secs(1);

/// How long Alt or Ctrl is held alone before the sidebar shows the numbers it
/// opens chats or projects with, so quick shortcuts don't flash them.
const REVEAL_NUMBERS_AFTER: Duration = Duration::from_millis(400);

/// Said above a chat whose last session couldn't be resumed, and kept in that
/// session's history.
const TRANSCRIPT_GONE: &str = "This session's transcript is gone, so it couldn't be resumed. \
                               Its saved history is still available here.";

/// Binds the shortcuts that reach the workspace.
pub fn init(cx: &mut App) {
    actions::init(cx);
}

pub struct Workspace {
    sidebar: Entity<Sidebar>,
    store: Store,
    /// How many changes show that aren't saved yet. The projects are listed
    /// again only once none are, as a listing from before one is saved would
    /// take it back.
    unsaved: usize,
    /// Listing the projects again, which a newer listing replaces.
    _listing: Option<Task<()>>,
    /// A project to select once the projects are listed, as one just added
    /// isn't before.
    select_listed: Option<i64>,
    /// Open chats, including exited terminals and history whose CLI couldn't
    /// start. Running processes keep running while another chat is shown,
    /// until the user closes them.
    chats: Vec<Chat>,
    /// The chat that is shown.
    active: Option<i64>,
    /// The chats on their way in or out, the ones being deleted, and which
    /// of them may take the view.
    lifecycle: Lifecycle,
    reports: Reports,
    codex_watcher: Option<notify::RecommendedWatcher>,
    /// What the watcher looks at: Codex's session folder, or while it doesn't
    /// exist yet, the closest folder above it.
    codex_watch: Option<(PathBuf, RecursiveMode)>,
    _sidebar_events: Subscription,
    _report_events: Task<()>,
    _codex_events: Task<()>,
    /// Brings the shown chat's turn up to date while it's under way.
    turn_tick: Option<Task<()>>,
    /// Shows the sidebar's shortcut numbers once their modifier has been
    /// held a moment on its own.
    reveal_numbers: Option<Task<()>>,
    /// The agents the selected project can start, as last looked up off the
    /// UI thread, and the lookup under way.
    installed: Option<(i64, Vec<Agent>)>,
    _installed_lookup: Option<Task<()>>,
    _new_chat_lookup: Option<Task<()>>,
    /// The search box last opened, with the project whose agents it offers
    /// to start, which fill in once they have been looked up.
    palette_start: Option<(i64, palette::Sections)>,
    _theme: Subscription,
    _focus_lost: Subscription,
    /// Why the workspace can't be used: the database didn't open, or another
    /// Agents Hub holds the lock.
    broken: Option<SharedString>,
    /// The sessions whose transcript is being read, which shutdown reads
    /// itself since it can't wait for them. A session is in it once per read
    /// under way, so it stays until the last of them is done.
    reading: Vec<i64>,
    /// The chats whose last session is to be read once the sessions asked to
    /// be recorded are, which shutdown reads itself instead. A chat is in it
    /// once per read.
    reading_last: Vec<i64>,
    /// The worktrees being checked out for new chats. Each is taken by its
    /// chat once git is done, or by shutdown, so none is left without a chat
    /// to remove it.
    checkouts: Vec<worktree::Checkout>,
    /// The lock that keeps a second instance off the database.
    _lock: Option<fs::File>,
    /// The projects' shells, by folder, for quick commands next to their
    /// chats. Nothing about them is saved, and they run until they exit or
    /// the app quits.
    shells: HashMap<PathBuf, Shell>,
    /// Whether the settings show in place of the chat.
    settings_shown: bool,
    /// The running chats' menu, while it is open.
    running_menu: Option<Entity<PopupMenu>>,
    /// Whether the window is closed to the tray, so no terminal is shown.
    hidden: bool,
    /// The window last opened on the workspace, for work that finishes
    /// without one, like a chat closing once a Codex lookup is done, to give
    /// the keyboard back.
    window: AnyWindowHandle,
    /// Holds the focus when nothing in the workspace does, so its key
    /// bindings still reach it.
    focus_handle: FocusHandle,
    /// Tracks the shown chat, so closing it can tell whether the keyboard
    /// went with it, from its CLI or from its header's buttons alike.
    chat_focus: FocusHandle,
    /// How wide the user wants the sidebar, saved once a drag of its edge
    /// ends. A window too narrow for it shows it narrower.
    sidebar_width: Pixels,
    /// Whether a database to import is being picked or checked.
    importing: bool,
}

/// Takes the worktree out of `job` and removes it, unless it was taken.
fn take_and_remove(
    job: &Mutex<Option<(Worktree, PathBuf)>>,
) -> Option<anyhow::Result<worktree::Removal>> {
    lifecycle::take_and_remove(job, worktree::remove)
}

/// What dragging the sidebar's edge carries, and draws: nothing, since the
/// sidebar itself follows the pointer.
#[derive(Clone)]
struct ResizeSidebar;

impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What dragging the rail's edge carries, and draws: nothing, as for the
/// sidebar's.
#[derive(Clone)]
struct ResizeRail;

impl Render for ResizeRail {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What dragging the shell's top edge carries, and draws: nothing, as for the
/// sidebar's.
#[derive(Clone)]
struct ResizeShell;

impl Render for ResizeShell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// When the user asked for a new chat, which what finishes later, git's
/// answers and the checkout of the chat's worktree, is checked against.
#[derive(Clone, Copy, PartialEq)]
struct Asked {
    navigations: u64,
    closed_all: u64,
}

/// Where the agents to start are among the search box's sections.
const PALETTE_START: usize = 1;

/// The search box's entries that start `agents` in a new chat of project
/// `project_id`.
fn start_entries(project_id: i64, agents: &[Agent]) -> Vec<Entry> {
    agents
        .iter()
        .map(|&agent| {
            Entry::new(
                agent.display_name(),
                Lead::Icon(IconName::Plus.into()),
                Pick::StartAgent(project_id, agent),
            )
            .meta(agent.program())
            .keywords(["new chat", "start"])
        })
        .collect()
}

/// The shell's height in a window `viewport` tall, taking `share` of it but
/// kept within the shell's limits.
fn shell_height(share: f32, viewport: Pixels) -> Pixels {
    (viewport * share.min(MAX_SHELL_SHARE)).max(MIN_SHELL_HEIGHT)
}

/// The share of a window `viewport` tall that a shell dragged `height` tall
/// takes, kept within the shell's limits.
fn shell_share(height: Pixels, viewport: Pixels) -> f32 {
    if viewport <= px(0.) {
        return DEFAULT_SHELL_SHARE;
    }
    let max = (viewport * MAX_SHELL_SHARE).max(MIN_SHELL_HEIGHT);
    height.clamp(MIN_SHELL_HEIGHT, max) / viewport
}

/// The shell's top edge: a stronger line than the app's other borders, so a
/// shell open under the chat reads as apart from it. Engaged, it wears the
/// app's usual handle.
fn shell_edge_appearance(strong: Hsla) -> ResizeHandleRenderer {
    let usual = resize_handle_appearance();
    Rc::new(move |handle, window, cx| {
        // Always drawn, so its indicator's transitions keep sampling.
        let usual = usual(handle, window, cx)?;
        Some(
            div()
                .flex_none()
                .relative()
                .w_full()
                .h(px(1.))
                .child(usual)
                .when(handle.state() == ResizeHandleState::Idle, |this| {
                    this.child(div().absolute().inset_0().bg(strong))
                })
                .into_any_element(),
        )
    })
}

/// The most room the rail has in a window `window_width` wide: what leaves
/// the rest of the sidebar and the chat their least room.
pub fn rail_room(window_width: Pixels) -> Pixels {
    window_width - MIN_MAIN_WIDTH - MIN_SIDEBAR_WIDTH + sidebar::RAIL_WIDTH
}

/// `width` kept within the sidebar's limits, and narrow enough to leave the
/// chat room in a window `window_width` wide. The width and the limits leave
/// out `names`, the room the projects' names take in the rail, which the
/// sidebar has on top.
fn clamp_sidebar_width(width: Pixels, window_width: Pixels, names: Pixels) -> Pixels {
    let max = MAX_SIDEBAR_WIDTH
        .min(window_width - MIN_MAIN_WIDTH - names)
        .max(MIN_SIDEBAR_WIDTH);
    width.clamp(MIN_SIDEBAR_WIDTH, max)
}

struct Shell {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    /// Whether the panel shows this shell. Each project keeps its own, so
    /// switching projects leaves the panel as it was in each of them.
    shown: bool,
    _exited: Subscription,
}

/// Sessions a chat's CLI went into, as one report or lookup found them.
#[derive(Clone)]
struct Recording {
    /// Closes once they're recorded, as are those asked for before.
    done: smol::channel::Receiver<()>,
    /// The last of them, once recorded. A newer recording can finish first,
    /// so what the chat's CLI was in when this was asked for is this, not the
    /// chat's `session`.
    session: Rc<Cell<Option<i64>>>,
    /// The agent's id for that session, which is the chat's before it's
    /// recorded.
    agent_id: String,
}

struct Chat {
    id: i64,
    agent: Agent,
    /// Its project's folder, which its CLI runs in.
    cwd: PathBuf,
    /// The name of the file its CLI reports its sessions to.
    report: String,
    terminal: Option<ChatTerminal>,
    /// Startup failure or exit message, shown alongside the saved history.
    message: Option<SharedString>,
    history: Entity<History>,
    /// The session the CLI is in, once known. It changes when the CLI
    /// switches to another session, as on `/clear`.
    session: Option<i64>,
    /// That session's transcript.
    transcript: Option<PathBuf>,
    /// The sessions the CLI last went into, being recorded.
    recording: Option<Recording>,
    /// What the CLI last reported doing, if it reports that at all.
    status: Option<Status>,
    /// How long the CLI has been on the last prompt, or took on it.
    turn: Option<Turn>,
    /// Whether the CLI finished while another chat was shown, and hasn't been
    /// looked at since.
    unseen: bool,
    /// The lookups of the session its CLI is in, if it's Codex.
    codex: Codex,
    /// Something about the session to show above the terminal, without a
    /// restart button.
    notice: Option<SharedString>,
    /// Whether restarting starts a new session instead of resuming the last.
    fresh_on_restart: bool,
}

/// A chat being added, with what starting it needs once it is.
#[derive(Clone)]
struct PendingChat {
    project_id: i64,
    agent: Agent,
    project: PathBuf,
    saving: Saving,
    asked: Asked,
}

struct ChatTerminal {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    exited: bool,
    _exited: Subscription,
    _status: Subscription,
}

impl Chat {
    fn running(&self) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|terminal| !terminal.exited)
    }

    fn current_session(&self) -> Option<i64> {
        self.session.filter(|_| self.running())
    }
}

impl Workspace {
    pub fn new(opened: Opened, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Opened {
            store,
            lock,
            imported,
            settings,
            projects,
            broken,
        } = opened;
        // Said once the window is up, since its notifications aren't yet.
        if let Some(imported) = imported {
            let message = match imported {
                store::Imported::Replaced(Some(backup)) => format!(
                    "Imported the database. The one it replaced is backed up at {}",
                    home_relative(&backup)
                ),
                store::Imported::Replaced(None) => "Imported the database.".into(),
                store::Imported::Failed(err) => {
                    eprintln!("can't import the database: {err:#}");
                    format!("Couldn't import the database, so it is as it was: {err:#}")
                }
            };
            cx.spawn_in(window, async move |_, cx| {
                cx.update(|window, cx| window.push_notification(message, cx))
                    .ok();
            })
            .detach();
        }

        cx.set_global(settings);
        appearance::apply_motion(cx);
        let sidebar = cx.new(Sidebar::new);
        let sidebar_events =
            cx.subscribe_in(&sidebar, window, |this, _, event, window, cx| match event {
                SidebarEvent::AddProject => this.add_project(window, cx),
                SidebarEvent::DropPaths(paths) => this.add_dropped(paths, window, cx),
                SidebarEvent::RemoveProject(id) => this.confirm_remove_project(*id, window, cx),
                SidebarEvent::ArchiveProject(id) => this.confirm_archive_project(*id, window, cx),
                SidebarEvent::NewChat(project_id) => {
                    this.open_new_chat_dialog(*project_id, window, cx)
                }
                SidebarEvent::OpenChat(id) => this.open_chat(*id, window, cx),
                SidebarEvent::RenameChat(id, title) => {
                    this.open_rename_dialog(*id, title.clone(), window, cx)
                }
                SidebarEvent::PinChat(id, pinned) => this.change(
                    Change::Pin {
                        chat: *id,
                        pinned: *pinned,
                    },
                    cx,
                ),
                SidebarEvent::MoveChat(id, to) => {
                    this.change(Change::MoveChat { chat: *id, to: *to }, cx)
                }
                SidebarEvent::MoveProject(id, to) => this.change(
                    Change::MoveProject {
                        project: *id,
                        to: *to,
                    },
                    cx,
                ),
                SidebarEvent::CloseChat(id) => this.confirm_close_chat(*id, window, cx),
                SidebarEvent::DeleteChat(id) => this.confirm_delete_chat(*id, window, cx),
                SidebarEvent::ArchiveChat(id) => this.confirm_archive_chat(*id, window, cx),
                SidebarEvent::RestoreChat(id) => this.change(Change::RestoreChat(*id), cx),
                SidebarEvent::OpenSettings => this.show_settings(true, window, cx),
                SidebarEvent::ShowProjectNames(show) => this.show_project_names(*show, window, cx),
                SidebarEvent::Selected => this.project_selected(window, cx),
            });

        let (reports, reported) = Reports::new();
        let report_events = cx.spawn(async move |this, cx| {
            while let Ok(key) = reported.recv().await {
                if this
                    .update(cx, |this, cx| this.read_reports(&key, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let (codex_watcher, codex_events) = codex::watch_sessions(cx);

        // The workspace outlives its window (see `tray`), so it's released
        // when the app quits.
        cx.on_release(|this, cx| this.shutdown(cx)).detach();

        let theme = cx.observe_global::<Theme>(|this, cx| {
            let colors = appearance::terminal_colors(cx);
            for chat in &this.chats {
                if let Some(terminal) = &chat.terminal {
                    terminal.terminal.read(cx).set_colors(colors);
                }
            }
            for shell in this.shells.values() {
                shell.terminal.read(cx).set_colors(colors);
            }
        });

        // Nothing has the keyboard once what had it is gone, such as the
        // terminal of a chat closed from a dialog, which hands the focus back
        // to that terminal as it goes. The shortcuts would stop working, so
        // the shown chat, or the workspace, takes it.
        let focus_lost = cx.on_focus_lost(window, |this, window, cx| this.focus_chat(window, cx));

        let mut this = Self {
            sidebar,
            store,
            unsaved: 0,
            _listing: None,
            select_listed: None,
            chats: Vec::new(),
            active: None,
            lifecycle: Lifecycle::default(),
            reports,
            codex_watcher,
            codex_watch: None,
            _sidebar_events: sidebar_events,
            _report_events: report_events,
            _codex_events: codex_events,
            turn_tick: None,
            reveal_numbers: None,
            installed: None,
            _installed_lookup: None,
            _new_chat_lookup: None,
            palette_start: None,
            _theme: theme,
            _focus_lost: focus_lost,
            broken: broken.map(SharedString::from),
            reading: Vec::new(),
            reading_last: Vec::new(),
            checkouts: Vec::new(),
            _lock: lock,
            shells: HashMap::new(),
            settings_shown: false,
            running_menu: None,
            hidden: false,
            window: window.window_handle(),
            focus_handle: cx.focus_handle(),
            chat_focus: cx.focus_handle(),
            sidebar_width: px(cx.global::<Settings>().sidebar_width),
            importing: false,
        };
        this.update_codex_watch();
        this.sidebar
            .update(cx, |sidebar, cx| sidebar.set_projects(projects, cx));
        this.catch_up(cx);
        this
    }

    /// Reads what the CLIs wrote since the transcripts were last read, so
    /// that the search finds it without each chat having to be opened first.
    fn catch_up(&self, cx: &mut Context<Self>) {
        let behind = self.store.transcripts_behind();
        cx.spawn(async move |this, cx| {
            match behind.await {
                Ok(sessions) => this
                    .update(cx, |this, cx| {
                        this.read_sessions(sessions, false, cx).detach()
                    })
                    .ok(),
                Err(err) => {
                    eprintln!("can't list the transcripts to read: {err}");
                    None
                }
            };
        })
        .detach();
    }





    /// Says so if a setting couldn't be saved, which then lasts until Agents
    /// Hub quits.
    fn save_setting(
        &self,
        saved: impl Future<Output = anyhow::Result<()>> + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Err(err) = saved.await {
                eprintln!("can't save the settings: {err:#}");
                let message =
                    format!("Couldn't save the setting, so it lasts until Agents Hub quits: {err}");
                this.update(cx, |this, cx| this.say(message, cx)).ok();
            }
        })
        .detach();
    }

    /// Says `message` in the window, unless it is closed to the tray.
    fn say(&self, message: String, cx: &mut App) {
        self.window
            .update(cx, |_, window, cx| window.push_notification(message, cx))
            .ok();
    }

    /// Says `message` with a button that makes the `undo` change.
    fn offer_undo(
        &self,
        message: &'static str,
        undo: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        window.push_notification(
            Notification::new()
                .message(message)
                .action(move |_, _, cx| {
                    let (this, undo) = (this.clone(), undo.clone());
                    Button::new("undo").label("Undo").on_click(cx.listener(
                        move |note, _, window, cx| {
                            this.update(cx, |this, cx| this.change(undo.clone(), cx))
                                .ok();
                            note.dismiss(window, cx);
                        },
                    ))
                })
                // A button keeps it up unless it's told otherwise.
                .autohide(true),
            cx,
        );
    }



    /// Focuses the shown chat's CLI, or the workspace if it has none running
    /// or the settings cover it. A terminal out of the rendered tree can't
    /// hold the focus: keys would skip the workspace's bindings.
    pub fn focus_chat(&self, window: &mut Window, cx: &mut App) {
        match self
            .chats
            .iter()
            .find(|chat| Some(chat.id) == self.active)
            .and_then(|chat| chat.terminal.as_ref())
            .filter(|_| !self.settings_shown)
        {
            Some(terminal) => terminal.view.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }


}


impl Workspace {
    /// The sidebar's right edge, which resizes it when dragged and restores
    /// its default width when double clicked.
    fn render_sidebar_edge(&self, cx: &Context<Self>) -> impl IntoElement {
        let workspace = cx.weak_entity();
        resize_handle("sidebar-edge", Axis::Horizontal)
            .inside(HandleEdge::Trailing)
            .on_drag(ResizeSidebar, move |drag, _, _, cx| {
                cx.stop_propagation();
                let workspace = workspace.clone();
                cx.new(|cx| {
                    // The drag lets go of what it carries once it ends, which
                    // is when the width the user settled on is saved.
                    cx.on_release(move |_, cx| {
                        workspace
                            .update(cx, |this, cx| this.save_sidebar_width(cx))
                            .ok();
                    })
                    .detach();
                    drag.clone()
                })
            })
            .on_double_click(cx.listener(|this, _, _, cx| {
                this.sidebar_width = px(DEFAULT_SIDEBAR_WIDTH);
                this.save_sidebar_width(cx);
                cx.notify();
            }))
    }

    /// The rail's right edge, which resizes the rail when dragged, folds it
    /// to the projects' marks when dragged narrow enough, and restores its
    /// default width when double clicked. The sidebar grows or shrinks with
    /// it, so its list keeps its width.
    fn render_rail_edge(&self, window_width: Pixels, cx: &Context<Self>) -> impl IntoElement {
        let workspace = cx.weak_entity();
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(sidebar::rail(window_width, cx).1)
            .child(
                resize_handle("rail-edge", Axis::Horizontal)
                    .inside(HandleEdge::Trailing)
                    .on_drag(ResizeRail, move |drag, _, _, cx| {
                        cx.stop_propagation();
                        let workspace = workspace.clone();
                        cx.new(|cx| {
                            // Saved once the drag ends, as the sidebar's width.
                            cx.on_release(move |_, cx| {
                                workspace.update(cx, |this, cx| this.save_rail(cx)).ok();
                            })
                            .detach();
                            drag.clone()
                        })
                    })
                    .on_double_click(cx.listener(|this, _, _, cx| {
                        this.resize_rail(true, DEFAULT_RAIL_WIDTH, cx);
                        this.save_rail(cx);
                    })),
            )
    }

    /// Shows the rail `width` wide, naming the projects, or with their marks
    /// alone, without saving it.
    fn resize_rail(&mut self, named: bool, width: f32, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        if settings.project_names == named && settings.rail_width == width {
            return;
        }
        cx.set_global(Settings {
            project_names: named,
            rail_width: width,
            ..settings
        });
        self.sidebar.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn save_rail(&mut self, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        let width = Settings::set_rail_width(&self.store, settings.rail_width, cx);
        let names = Settings::set_project_names(&self.store, settings.project_names, cx);
        self.save_setting(
            async move {
                width.await?;
                names.await
            },
            cx,
        );
    }

    /// Shows the shell taking `share` of the window's height, without saving
    /// it.
    fn resize_shell(&mut self, share: f32, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        if settings.shell_share == share {
            return;
        }
        cx.set_global(Settings {
            shell_share: share,
            ..settings
        });
        cx.notify();
    }

    fn save_shell_share(&mut self, cx: &mut Context<Self>) {
        let share = cx.global::<Settings>().shell_share;
        let saved = Settings::set_shell_share(&self.store, share, cx);
        self.save_setting(saved, cx);
    }

    fn save_sidebar_width(&mut self, cx: &mut Context<Self>) {
        let width = f32::from(self.sidebar_width);
        if cx.global::<Settings>().sidebar_width == width {
            return;
        }
        let saved = Settings::set_sidebar_width(&self.store, width, cx);
        self.save_setting(saved, cx);
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_width = window.viewport_size().width;
        let names = sidebar::names_width(window_width, cx);
        let room =
            window_width - clamp_sidebar_width(self.sidebar_width, window_width, names) - names;
        let settings = self
            .settings_shown
            .then(|| render_settings(room, window, cx).into_any_element());
        let theme = cx.theme();
        if let Some(reason) = self.broken.clone() {
            return render_broken(reason, cx);
        }
        let active = self
            .active
            .and_then(|id| self.chats.iter().find(|chat| chat.id == id));

        h_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_shell))
            .on_action(cx.listener(Self::open_search))
            .on_action(cx.listener(Self::new_chat_here))
            .on_action(cx.listener(Self::add_project_action))
            .on_action(cx.listener(Self::toggle_settings))
            .on_action(cx.listener(Self::toggle_history))
            .on_action(cx.listener(Self::rename_active))
            .on_action(cx.listener(Self::toggle_pin))
            .on_action(cx.listener(Self::delete_active))
            .on_action(cx.listener(Self::archive_active))
            .on_action(cx.listener(Self::archive_selected))
            .on_action(cx.listener(Self::show_archived))
            .on_action(cx.listener(Self::close_all_chats))
            .on_action(cx.listener(Self::close_idle_chats))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::toggle_project_names))
            .on_action(cx.listener(Self::use_theme))
            .on_action(cx.listener(Self::export_action))
            .on_action(cx.listener(Self::import_action))
            .on_action(cx.listener(Self::open_chat_at))
            .on_action(cx.listener(Self::select_project_at))
            .on_action(cx.listener(Self::show_running))
            .on_action(cx.listener(Self::next_needing_you))
            .on_action(cx.listener(Self::show_shortcuts))
            .on_key_down(cx.listener(Self::empty_key))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.modifiers_changed(event.modifiers, cx)
            }))
            // Seen before the terminal takes it: a key pressed with the
            // modifier is a shortcut, not a look at the numbers.
            .capture_key_down(
                cx.listener(|this, _: &KeyDownEvent, _, cx| this.release_modifiers(cx)),
            )
            .on_action(cx.listener(|this, _: &terminal_view::ZoomIn, window, cx| {
                this.zoom_terminals(1., window, cx)
            }))
            .on_action(cx.listener(|this, _: &terminal_view::ZoomOut, window, cx| {
                this.zoom_terminals(-1., window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &terminal_view::ResetZoom, window, cx| {
                    let reset =
                        DEFAULT_TERMINAL_FONT_SIZE - cx.global::<Settings>().terminal_font_size;
                    this.zoom_terminals(reset, window, cx)
                }),
            )
            // Follows the pointer anywhere in the window while the sidebar's
            // edge is dragged, since it leaves the edge's thin band at once.
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeSidebar>, window, cx| {
                    let window_width = window.viewport_size().width;
                    let names = sidebar::names_width(window_width, cx);
                    let width = clamp_sidebar_width(
                        event.event.position.x - event.bounds.left() - names,
                        window_width,
                        names,
                    );
                    if width != this.sidebar_width {
                        this.sidebar_width = width;
                        cx.notify();
                    }
                }),
            )
            // The rail's edge likewise. The rail starts where the window
            // does.
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeRail>, window, cx| {
                    let width = event.event.position.x - event.bounds.left();
                    let rail_width = cx.global::<Settings>().rail_width;
                    if width < sidebar::FOLD_RAIL_BELOW {
                        // Folded, it keeps the width to unfold to.
                        this.resize_rail(false, rail_width, cx);
                    } else {
                        let max = rail_room(window.viewport_size().width)
                            .min(sidebar::MAX_RAIL_WIDTH)
                            .max(sidebar::MIN_NAMED_RAIL_WIDTH);
                        let width = width.clamp(sidebar::MIN_NAMED_RAIL_WIDTH, max);
                        this.resize_rail(true, f32::from(width), cx);
                    }
                }),
            )
            .child(
                div()
                    .relative()
                    .w(clamp_sidebar_width(self.sidebar_width, window_width, names) + names)
                    .h_full()
                    .flex_none()
                    // Terminal output re-renders the window many times a
                    // second; the sidebar and the history only need to render
                    // again when their own state changes.
                    .child(
                        self.sidebar
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    )
                    .child(self.render_rail_edge(window_width, cx))
                    .child(self.render_sidebar_edge(cx)),
            )
            // The sheet the chat is drawn on, set on the sidebar's ground
            // under the toolbar.
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_toolbar(room, window, cx))
                    .child(
                        v_flex().flex_1().min_h_0().px_2().pb_2().child(
                            v_flex()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .bg(theme.background)
                                .border_1()
                                .border_color(theme.border)
                                .rounded(theme.radius_lg)
                                .shadow_xs()
                                .child(v_flex().flex_1().min_h_0().map(|this| {
                                    match (settings, active) {
                                        (Some(settings), _) => this.child(settings),
                                        (None, Some(chat)) => {
                                            this.child(self.render_chat(chat, window, cx))
                                        }
                                        (None, None) => this.child(
                                            self.render_over_shell(
                                                div()
                                                    .size_full()
                                                    .overflow_hidden()
                                                    .child(self.render_empty(window, cx)),
                                                px(0.),
                                                window,
                                                cx,
                                            ),
                                        ),
                                    }
                                }))
                                .child(self.render_footer(room, window, cx)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Workspace {
    fn render_chat(&self, chat: &Chat, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = chat.id;
        let viewport = window.viewport_size().height;
        let shell_height = shell_height(cx.global::<Settings>().shell_share, viewport);
        let shell = self.shown_shell(cx);
        // Without a terminal, the history takes the sheet but for its header
        // and footer, and the shell.
        let history_max = if chat.terminal.is_none() {
            viewport
                - px(140.)
                - if shell.is_some() {
                    shell_height
                } else {
                    px(0.)
                }
        } else {
            viewport * HISTORY_SHARE
        };

        // A spinner only earns its frames while the window is on screen. The
        // sidebar's glyphs stay still: each tick would rebuild the whole list.
        let spin = window.is_visible() && !self.hidden;
        // A window too short for even the least heights clips the chat
        // rather than drawing it under the footer.
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.chat_focus)
            .child(self.render_chat_header(chat, spin, cx))
            .when_some(chat.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(notice),
                )
            })
            .when_some(chat.message.clone(), |this, message| {
                let tint = appearance::palette(cx).danger;
                this.child(
                    h_flex()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .gap_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(tint.soft)
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(tint.text)
                                .child(message),
                        )
                        .child(
                            Button::new("restart-agent")
                                .small()
                                .label("Restart agent")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.restart(id, window, cx);
                                })),
                        ),
                )
            })
            .child(
                self.render_over_shell(
                    // The earlier sessions ask for a share of the window, but
                    // shrink to fit what the header, notices, shell and footer
                    // leave the CLI, which keeps its least height.
                    v_flex()
                        .size_full()
                        .children(chat.history.read(cx).height(history_max).map(|height| {
                            chat.history.clone().cached(
                                StyleRefinement::default()
                                    .w_full()
                                    .h(height)
                                    .flex_shrink(1.)
                                    .min_h_0(),
                            )
                        }))
                        .children(chat.terminal.as_ref().map(|terminal| {
                            // With the shell beside it, which of the two has
                            // the keyboard.
                            let focused = shell.is_some()
                                && terminal.view.focus_handle(cx).is_focused(window);
                            div()
                                .relative()
                                .flex_1()
                                .min_h(MIN_TERMINAL_HEIGHT)
                                .pl_5()
                                .pr_2()
                                .pt_3()
                                .when(focused, |this| this.child(focus_bar(cx)))
                                .child(terminal.view.clone())
                        })),
                    if chat.terminal.is_some() {
                        MIN_TERMINAL_HEIGHT
                    } else {
                        px(0.)
                    },
                    window,
                    cx,
                ),
            )
    }

    /// `above` filling what the shell leaves of the room, with the shell, when
    /// it shows, at the room's bottom. The shell keeps the height the user
    /// dragged it to, and what's above gives up its room to it but for
    /// `above_min`; only in a room too short for both does the shell shrink.
    fn render_over_shell(
        &self,
        above: impl IntoElement,
        above_min: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_h_0()
            // Clips the shell as it slides in from the bottom.
            .overflow_hidden()
            // Follows the pointer anywhere in the window while the shell's
            // top edge is dragged. The shell grows up from the room's bottom,
            // which stays put however tall it is.
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<ResizeShell>, window, cx| {
                    let room = event.bounds.size.height - above_min;
                    let height = (event.bounds.bottom() - event.event.position.y).min(room);
                    let share = shell_share(height, window.viewport_size().height);
                    this.resize_shell(share, cx);
                },
            ))
            .child(div().flex_1().min_h(above_min).child(above))
            .children(self.render_shell(window, cx))
    }

    /// The shell, under the chat or what shows without one, when it shows.
    fn render_shell(&self, window: &Window, cx: &Context<Self>) -> Option<impl IntoElement> {
        let theme = cx.theme();
        let (cwd, shell) = self.shown_shell(cx)?;
        let viewport = window.viewport_size().height;
        let workspace = cx.weak_entity();
        let height = shell_height(cx.global::<Settings>().shell_share, viewport);
        let focused = shell.view.focus_handle(cx).is_focused(window);
        // Named after its folder, so each project's shell slides in when it
        // first shows.
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&cwd, &mut hasher);
        let key = std::hash::Hasher::finish(&hasher);
        Some(
            v_flex()
                .relative()
                .flex_shrink(1.)
                .h(height)
                .min_h(MIN_SHELL_HEIGHT)
                .child(
                    h_flex()
                        .flex_none()
                        .h_8()
                        .pl_5()
                        .pr_2()
                        .gap_2()
                        .bg(theme.muted)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_medium()
                                .text_color(theme.muted_foreground)
                                .child("Shell"),
                        )
                        .child(div().flex_1().min_w_0().child(sidebar::path_line(&cwd, cx)))
                        .child(
                            Button::new("close-shell")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip_with_action("Hide the shell", &ToggleShell, None)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_shell(&ToggleShell, window, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .pl_5()
                        .pr_2()
                        .when(focused && self.active.is_some(), |this| {
                            this.child(focus_bar(cx))
                        })
                        .child(shell.view.clone()),
                )
                // Last, so it lies over the header it edges. It resizes the
                // shell when dragged, saving its height once the drag ends,
                // and restores its default height when double clicked.
                .child(
                    resize_handle("shell-edge", Axis::Vertical)
                        .inside(HandleEdge::Leading)
                        .with_appearance(shell_edge_appearance(
                            appearance::palette(cx).border_strong,
                        ))
                        .on_drag(ResizeShell, move |drag, _, _, cx| {
                            cx.stop_propagation();
                            let workspace = workspace.clone();
                            cx.new(|cx| {
                                cx.on_release(move |_, cx| {
                                    workspace
                                        .update(cx, |this, cx| this.save_shell_share(cx))
                                        .ok();
                                })
                                .detach();
                                drag.clone()
                            })
                        })
                        .on_double_click(cx.listener(|this, _, _, cx| {
                            this.resize_shell(DEFAULT_SHELL_SHARE, cx);
                            this.save_shell_share(cx);
                        })),
                )
                // Up from the sheet's bottom edge, which clips it, into the
                // room it takes at once, so its terminal sizes only once.
                .with_animation(
                    ("shell-in", key),
                    Animation::new(SHELL_IN).with_easing(ease_out_quint()),
                    move |shell, shown| shell.top(height * (1. - shown)),
                ),
        )
    }

    /// The chat's title and folder, what its agent is doing, and what can be
    /// done with it.
    fn render_chat_header(&self, chat: &Chat, spin: bool, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let entry = self.active_entry(cx);
        let title = entry
            .and_then(|entry| entry.title.clone())
            .unwrap_or_else(|| "New chat".into());
        let pinned = entry.is_some_and(|entry| entry.pinned);
        let tone = Tone::of(self.sidebar.read(cx).running(chat.id));
        let history = chat.history.read(cx);
        let (earlier, open) = (history.count(), history.is_open());
        let focus = self.focus_handle.clone();

        // A sheet too narrow for the title, the status and the actions on one
        // line wraps them, rather than pushing the actions out of reach.
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap_x_4()
            .gap_y_2()
            .pl_6()
            .pr_3()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(MIN_TITLE_WIDTH)
                    .child(
                        div()
                            .font_family(appearance::DISPLAY)
                            .text_xl()
                            .truncate()
                            .child(title),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex_none()
                                    .child(format!("{} · ", chat.agent.label())),
                            )
                            .child(sidebar::path_line(&chat.cwd, cx)),
                    ),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .flex_wrap()
                    .justify_end()
                    .gap_x_4()
                    .gap_y_2()
                    .when(chat.running(), |this| {
                        this.child(status_pill(tone, chat.turn, spin, cx))
                    })
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when(earlier > 0, |this| {
                                this.child(
                                    Button::new("history")
                                        .ghost()
                                        .small()
                                        .icon(ExtraIcon::Clock)
                                        .label(format!("{earlier} earlier"))
                                        .selected(open)
                                        .tooltip_with_action(
                                            "Show or hide the earlier sessions",
                                            &ToggleHistory,
                                            None,
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_history(&ToggleHistory, window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("shell")
                                    .ghost()
                                    .small()
                                    .icon(IconName::SquareTerminal)
                                    .label("Shell")
                                    .selected(self.shell_visible(cx))
                                    .tooltip_with_action(
                                        "Show or hide the project's shell",
                                        &ToggleShell,
                                        None,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_shell(&ToggleShell, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("chat-menu")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Ellipsis)
                                    .tooltip("More")
                                    .dropdown_menu(move |menu, _, _| {
                                        menu.action_context(focus.clone())
                                            .menu("Rename", Box::new(RenameChat))
                                            .menu(
                                                if pinned { "Unpin" } else { "Pin to the top" },
                                                Box::new(TogglePin),
                                            )
                                            .separator()
                                            .menu("Archive", Box::new(ArchiveChat))
                                            .menu("Delete", Box::new(DeleteChat))
                                    }),
                            ),
                    ),
            )
    }

    /// The search, which opens the search modal, and the menu of the running
    /// chats, over the `width` the sidebar leaves.
    fn render_toolbar(
        &self,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let sidebar = self.sidebar.read(cx);
        let running: Vec<_> = sidebar
            .running_chats()
            .into_iter()
            .map(|(_, _, tone)| tone)
            .collect();
        let needing = running.iter().filter(|tone| tone.needs_you()).count();
        let working = running.contains(&Tone::Working);
        let count = match (needing, running.len()) {
            (0, 0) => None,
            (0, open) => Some(sidebar::badge(open, false, working, cx)),
            (needing, _) => Some(sidebar::badge(needing, true, false, cx)),
        };
        let menu = self.running_menu.clone();
        let workspace = cx.weak_entity();

        let search = Button::new("search")
            .outline()
            .flex_1()
            .min_w_0()
            .h_8()
            .bg(theme.background)
            .icon(IconName::Search)
            .accessibility_label("Search chats, messages, projects and commands")
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child("Search chats, messages, projects and commands"),
                    )
                    // Too narrow for both, the search keeps its name.
                    .children(
                        palette::shortcut(&OpenSearch, window)
                            .filter(|_| width >= MIN_SEARCH_KEYS_WIDTH)
                            .map(|keys| {
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded(px(4.))
                                    .bg(palette.strong)
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(palette.secondary_foreground)
                                    .child(keys)
                            }),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(false, window, cx)));

        let running = Button::new("running")
            .ghost()
            .h_8()
            .icon(ExtraIcon::Activity)
            .label("Running")
            .children(count)
            .dropdown_caret(true)
            .tooltip_with_action("The running chats of every project", &ShowRunning, None);

        TitleBar::new()
            .h(TITLE_BAR_HEIGHT)
            .pl_2()
            .bg(transparent_black())
            .border_b_0()
            .on_close_window(|_, window, cx| {
                tray::remember_bounds(window.window_bounds(), cx);
                window.remove_window();
            })
            // Over the title bar's row rather than in it: the row never gets
            // narrower than what's in it, which would push the window's
            // controls out of a narrow window.
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .gap_4()
                    .pr_2()
                    .justify_between()
                    .child(stay_put(h_flex().flex_1().min_w_0().max_w(px(480.))).child(search))
                    .child(
                        stay_put(div()).child(
                            Popover::new("running-menu")
                                .anchor(Anchor::TopRight)
                                .appearance(false)
                                // The menu closes itself on a click outside it.
                                .overlay_closable(false)
                                .open(menu.is_some())
                                .when_some(menu.as_ref(), |this, menu| {
                                    this.track_focus(&menu.focus_handle(cx))
                                })
                                .on_open_change(move |open, window, cx| {
                                    workspace
                                        .update(cx, |this, cx| {
                                            if *open {
                                                // Opened from the button, the popover
                                                // took the focus before the menu was there.
                                                this.open_running_menu(window, cx);
                                                if let Some(menu) = &this.running_menu {
                                                    menu.focus_handle(cx).focus(window, cx);
                                                }
                                            } else {
                                                this.running_menu = None;
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                })
                                .trigger(running)
                                .content(move |_, _, _| match &menu {
                                    Some(menu) => menu.clone().into_any_element(),
                                    None => div().into_any_element(),
                                }),
                        ),
                    ),
            )
    }

    /// What the agents are doing, and the shortcuts that help with it now,
    /// over a sheet `width` wide. The rest are a shortcut away.
    fn render_footer(
        &self,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let sidebar = self.sidebar.read(cx);
        let summary = sidebar.summary();
        let needing = sidebar.next_needing_you(None).is_some();
        let bound = |action: &dyn Action, label: &'static str| {
            palette::shortcut(action, window).map(|keys| key_hint(keys, label, cx))
        };

        h_flex()
            .flex_none()
            .h_8()
            .px_4()
            .gap_4()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .rounded_b(theme.radius_lg - px(1.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            // What the agents do comes first and is cut last; the hints
            // drop out whole rather than being cut mid-word.
            .child(div().flex_1().min_w_0().truncate().child(summary))
            .when(needing, |this| {
                this.children(bound(&NextNeedingYou, "next that needs you"))
            })
            .when(width >= MIN_FOOTER_SEARCH_WIDTH, |this| {
                this.children(bound(&OpenSearch, "search"))
            })
            .children(bound(&ShowShortcuts, "shortcuts"))
    }

    fn show_shortcuts(&mut self, _: &ShowShortcuts, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, window, cx| {
            dialog.title("Keyboard shortcuts").w(px(640.)).child(
                v_flex()
                    .id("shortcuts")
                    .max_h(px(480.))
                    .overflow_y_scroll()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .line_height(rems(1.4))
                            .text_color(cx.theme().muted_foreground)
                            .child(SHORTCUTS_WORK_ANYWHERE),
                    )
                    .child(render_shortcuts(window, cx)),
            )
        });
    }

    /// The agents the selected project can start now, in the new chat
    /// dialog's order.
    fn empty_agents(&self, cx: &App) -> Option<(i64, Vec<Agent>)> {
        let project = self.sidebar.read(cx).selected()?;
        let (id, agents) = self.installed.as_ref()?;
        if *id != project.id {
            return None;
        }
        Some((project.id, agents.clone()))
    }

    /// Starts the `n`th agent the empty state offers, counting from one, on
    /// its number key.
    fn empty_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_some()
            || self.settings_shown
            || event.keystroke.modifiers.number_of_modifiers() > 0
        {
            return;
        }
        let Some(n) = event.keystroke.key.parse::<usize>().ok() else {
            return;
        };
        if let Some((project, agents)) = self.empty_agents(cx)
            && let Some(agent) = n.checked_sub(1).and_then(|n| agents.get(n))
        {
            cx.stop_propagation();
            self.start_agent(project, *agent, window, cx);
        }
    }

    /// What shows while no chat is open: the agents to start, with their
    /// number keys, and the chats last worked in, to go back to.
    fn render_empty(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let sidebar = self.sidebar.read(cx);
        let selected = sidebar.selected().map(|project| project.name.clone());
        let agents = self.empty_agents(cx);
        let mut recent: Vec<_> = sidebar
            .projects()
            .iter()
            .filter(|project| project.archived_at.is_none())
            .flat_map(|project| {
                project
                    .chats
                    .iter()
                    .filter(|chat| chat.archived_at.is_none())
                    .map(move |chat| (project, chat))
            })
            .collect();
        recent.sort_by_key(|(_, chat)| std::cmp::Reverse(chat.updated_at));
        let recent: Vec<_> = recent
            .into_iter()
            .take(RECENT_CHATS)
            .map(|(project, chat)| {
                let tone = Tone::of(sidebar.running(chat.id));
                let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                (
                    chat.id,
                    title,
                    sidebar::subtitle(chat, tone, Some(&project.name)),
                    tone,
                )
            })
            .collect();

        // Centered while it fits, and scrolled from the top when it doesn't.
        div().id("empty").size_full().overflow_y_scroll().child(
            h_flex()
                .min_h_full()
                .items_center()
                .justify_center()
                .p_8()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(520.))
                        .gap_6()
                        .child(
                            v_flex()
                                .gap_2()
                                .child(div().font_family(appearance::DISPLAY).text_3xl().child(
                                    match &selected {
                                        Some(_) => "Start an agent",
                                        None => "Add a project to begin",
                                    },
                                ))
                                .child(
                                    div()
                                        .text_sm()
                                        .line_height(rems(1.45))
                                        .text_color(theme.muted_foreground)
                                        .child(match &selected {
                                            Some(name) => format!(
                                                "In {name}. Agents keep running while you look at \
                                             another chat."
                                            ),
                                            None => "A project is a folder you work in. Add one, \
                                                 then start Claude Code, Codex or Pi there."
                                                .to_string(),
                                        }),
                                ),
                        )
                        .child(match (&selected, agents) {
                            (Some(_), Some((project, agents))) if !agents.is_empty() => h_flex()
                                .flex_wrap()
                                .gap_2()
                                .children(agents.into_iter().enumerate().map(|(index, agent)| {
                                    Button::new(("empty-agent", index as u64))
                                        .map(|button| {
                                            if index == 0 {
                                                button.primary()
                                            } else {
                                                button.outline()
                                            }
                                        })
                                        .icon(IconName::Plus)
                                        .label(agent.display_name())
                                        .child(
                                            div()
                                                .px_1p5()
                                                .rounded(px(4.))
                                                .bg(palette.strong)
                                                .text_xs()
                                                .font_family(theme.mono_font_family.clone())
                                                .text_color(palette.secondary_foreground)
                                                .child((index + 1).to_string()),
                                        )
                                        .tooltip(format!(
                                            "Start {} here, or press {}",
                                            agent.display_name(),
                                            index + 1
                                        ))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.start_agent(project, agent, window, cx)
                                        }))
                                }))
                                .child(
                                    Button::new("empty-new-chat")
                                        .ghost()
                                        .label("More options…")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_chat_here(&NewChat, window, cx)
                                        })),
                                )
                                .into_any_element(),
                            (Some(_), _) => h_flex()
                                .child(
                                    Button::new("empty-new-chat")
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label("New chat")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_chat_here(&NewChat, window, cx)
                                        })),
                                )
                                .into_any_element(),
                            (None, _) => h_flex()
                                .child(
                                    Button::new("empty-add-project")
                                        .primary()
                                        .icon(IconName::FolderOpen)
                                        .label("Add project")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.add_project(window, cx)
                                        })),
                                )
                                .into_any_element(),
                        })
                        .when(!recent.is_empty(), |this| {
                            this.child(
                                v_flex()
                                    .gap_1()
                                    .pt_5()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .pb_1()
                                            .child(sidebar::heading("Recent", cx))
                                            .children(palette::shortcut(&OpenSearch, window).map(
                                                |keys| {
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child(format!("{keys} to search them all"))
                                                },
                                            )),
                                    )
                                    .children(recent.into_iter().map(
                                        |(id, title, about, tone)| {
                                            h_flex()
                                                .id(("recent", id as u64))
                                                .px_2()
                                                .py(px(7.))
                                                .mx(px(-8.))
                                                .gap(px(10.))
                                                .rounded(theme.radius)
                                                .cursor_pointer()
                                                .hover(|style| style.bg(theme.accent))
                                                .child(sidebar::dot(tone, cx))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_sm()
                                                        .truncate()
                                                        .child(title),
                                                )
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .max_w(px(240.))
                                                        .truncate()
                                                        .text_xs()
                                                        .font_family(theme.mono_font_family.clone())
                                                        .text_color(tone.text_color(cx))
                                                        .child(about),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.open_chat(id, window, cx)
                                                    },
                                                ))
                                        },
                                    )),
                            )
                        }),
                ),
        )
    }
}

const SHORTCUTS_WORK_ANYWHERE: &str = "They work while an agent has the keyboard, so they use \
                                        keys terminal programs leave alone.";

/// Every shortcut, with what it does.
fn render_shortcuts(window: &Window, cx: &App) -> impl IntoElement {
    let bound: [(&'static str, Box<dyn Action>); 12] = [
        (
            "Search chats, messages, projects and commands",
            Box::new(OpenSearch),
        ),
        (
            "Show the running chats of every project",
            Box::new(ShowRunning),
        ),
        (
            "Open the next chat that needs you",
            Box::new(NextNeedingYou),
        ),
        ("Start an agent in the selected project", Box::new(NewChat)),
        ("Show or hide the project's shell", Box::new(ToggleShell)),
        ("Show or hide the earlier sessions", Box::new(ToggleHistory)),
        ("Rename the chat", Box::new(RenameChat)),
        ("Add a project", Box::new(AddProject)),
        ("Open or close the settings", Box::new(ToggleSettings)),
        ("Close the idle chats", Box::new(CloseIdleChats)),
        ("Close all the chats", Box::new(CloseAllChats)),
        ("Show the keyboard shortcuts", Box::new(ShowShortcuts)),
    ];
    let unbound: [(&'static str, SharedString); 4] = [
        (
            "Open a chat by its place in the list; hold Alt to see the numbers",
            "Alt+1…9".into(),
        ),
        (
            "Select a project by its place in the rail; hold Ctrl to see the numbers",
            "Ctrl+1…9".into(),
        ),
        (
            "Make the terminals' text bigger or smaller",
            "Ctrl+scroll".into(),
        ),
        ("Copy or paste in a terminal", "Ctrl+Shift+C / V".into()),
    ];
    let rows = bound
        .iter()
        .filter_map(|(label, action)| {
            palette::shortcut(action.as_ref(), window).map(|keys| (*label, keys))
        })
        .chain(unbound);
    let theme = cx.theme();
    v_flex().children(rows.map(|(label, keys)| {
        h_flex()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(div().flex_1().min_w_0().child(label))
            .child(
                div()
                    .flex_none()
                    .px_1p5()
                    .rounded(px(4.))
                    .bg(appearance::palette(cx).strong)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(keys),
            )
    }))
}

/// The bar on the edge of the terminal that has the keyboard, while two show.
fn focus_bar(cx: &App) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .top_3()
        .bottom_3()
        .w(px(3.))
        .rounded_r(px(2.))
        .bg(appearance::palette(cx).accent)
}

/// Keeps a press on the title bar's controls from moving the window.
fn stay_put(element: Div) -> Div {
    element
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

/// A shortcut's keys, and what they do.
fn key_hint(keys: SharedString, label: &'static str, cx: &App) -> impl IntoElement {
    let palette = appearance::palette(cx);
    h_flex()
        .flex_none()
        .gap_1p5()
        .child(
            div()
                .px_1p5()
                .rounded(px(4.))
                .bg(palette.strong)
                .text_color(palette.secondary_foreground)
                .child(keys),
        )
        .child(label)
}

/// How long an agent has been on the user's last prompt, waiting on the user
/// included, or how long it took.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Turn {
    Since(Instant),
    Took(Duration),
}

impl Turn {
    /// The turn after an agent's status changes, from busy (working or
    /// waiting on the user) or not to busy or not.
    fn after(turn: Option<Self>, was_busy: bool, busy: bool, now: Instant) -> Option<Self> {
        match (was_busy, busy, turn) {
            (false, true, _) => Some(Self::Since(now)),
            (true, false, Some(Self::Since(start))) => Some(Self::Took(now - start)),
            (_, _, turn) => turn,
        }
    }
}

/// A duration in its two largest units, padded so it keeps its width while
/// it counts up.
pub fn duration(time: Duration) -> String {
    let seconds = time.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// What the agent is doing, on its status color, and how long its `turn` has
/// taken.
fn status_pill(tone: Tone, turn: Option<Turn>, spin: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let label = capitalized(tone.label());
    let (label, time) = match turn {
        Some(Turn::Since(start)) => (format!("{label} ·"), Some(duration(start.elapsed()))),
        Some(Turn::Took(took)) => (format!("{label} · took"), Some(duration(took))),
        None => (label, None),
    };
    let (background, foreground) = tone
        .tint(cx)
        .map(|tint| (tint.soft, tint.text))
        .unwrap_or((palette.strong, palette.secondary_foreground));
    h_flex()
        .flex_none()
        .h(px(26.))
        .px_2p5()
        .gap_2()
        .rounded_full()
        .bg(background)
        .text_color(foreground)
        .text_xs()
        .font_medium()
        .child(if spin {
            sidebar::live_dot(tone, cx)
        } else {
            sidebar::dot(tone, cx)
        })
        .child(h_flex().gap_1().child(label).when_some(time, |this, time| {
            // Figures of the same width, so the pill doesn't jitter as
            // they count up.
            this.child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .child(time),
            )
        }))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The sessions in a terminal's queued reports, taken out of them, in the
/// order its CLI went into them.
fn reported_sessions(
    reports: &mut Reports,
    key: &str,
    agent: Agent,
) -> Vec<(String, PathBuf, Option<String>)> {
    reports
        .read(key, agent)
        .into_iter()
        .map(|(agent_id, report)| (agent_id, report.transcript_path, report.reason))
        .collect()
}

/// Records the sessions a chat's CLI reported last, and reads `sessions`
/// along with the ones those reports moved the chat into or out of. One
/// transcript that can't be read doesn't stop the others.
fn import_chat(
    store: &Store,
    reports: &mut Reports,
    id: i64,
    agent: Agent,
    key: &str,
    mut sessions: Vec<i64>,
) -> anyhow::Result<()> {
    let mut failure = None;
    for (agent_id, report) in reports.read(key, agent) {
        let recorded =
            store.record_session(id, agent, agent_id, report.transcript_path, report.reason);
        match smol::block_on(recorded) {
            Ok(recorded) => {
                sessions.push(recorded.session_id);
                if let Some((_, read)) = recorded.left
                    && let Err(err) =
                        smol::block_on(read.recv()).unwrap_or_else(|err| Err(err.into()))
                {
                    failure.get_or_insert(err);
                }
            }
            Err(err) => {
                failure.get_or_insert(err);
            }
        }
    }
    sessions.sort_unstable();
    sessions.dedup();
    for session in sessions {
        if let Err(err) = smol::block_on(store.read_transcript(session)) {
            failure.get_or_insert(err);
        }
    }
    match failure {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// What shows when the database or the instance lock stopped the workspace
/// from opening. Nothing else is reachable, since nothing could be saved.
fn render_broken(reason: SharedString, cx: &Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_8()
        .bg(theme.background)
        .text_color(theme.foreground)
        .child(
            v_flex()
                .max_w(px(440.))
                .gap_4()
                .child(
                    div()
                        .text_xl()
                        .font_semibold()
                        .child("Agents Hub can't start"),
                )
                .child(
                    div()
                        .text_sm()
                        .line_height(rems(1.4))
                        .text_color(theme.muted_foreground)
                        .child(reason),
                )
                .child(
                    Button::new("quit")
                        .label("Quit")
                        .on_click(|_, _, cx| cx.quit()),
                ),
        )
        .into_any_element()
}

/// The projects of a database about to be imported, each with its first few
/// chats.
fn render_import(projects: &[store::Project], theme: &Theme) -> impl IntoElement {
    /// The chats listed under each project; the others are counted.
    const SHOWN_CHATS: usize = 3;
    v_flex()
        .id("import-projects")
        .max_h(px(260.))
        .overflow_y_scroll()
        .gap_3()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .children(projects.iter().map(|project| {
            let more = project.chats.len().saturating_sub(SHOWN_CHATS);
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_medium()
                                .text_color(theme.foreground)
                                .child(project.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(home_relative(&project.path)),
                        ),
                )
                .children(project.chats.iter().take(SHOWN_CHATS).map(|chat| {
                    div()
                        .pl_3()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(chat.title.clone().unwrap_or_else(|| "New chat".into()))
                }))
                .when(more > 0, |list| {
                    list.child(
                        div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child(format!("and {} more", plural(more, "chat"))),
                    )
                })
                .when(project.chats.is_empty(), |list| {
                    list.child(
                        div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child("No chats"),
                    )
                })
        }))
}

/// "1 chat", "2 chats".
fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Lifecycle, MAX_SHELL_SHARE, MIN_SHELL_HEIGHT, Turn, duration, import_chat,
        reported_sessions, shell_height, shell_share, take_and_remove,
    };
    use crate::store::{Db, Store};
    use crate::worktree::{Removal, Worktree};
    use crate::{agent::Agent, reports::Reports, transcript};
    use gpui_kit::px;
    use serde_json::json;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn times_turns_from_busy_to_at_rest() {
        let start = Instant::now();
        let later = start + Duration::from_secs(90);
        let since = Some(Turn::Since(start));
        let took = Some(Turn::Took(Duration::from_secs(5)));
        assert_eq!(Turn::after(None, false, true, start), since);
        // A new prompt starts over.
        assert_eq!(Turn::after(took, false, true, start), since);
        // Waiting on the user, or back at work, is still the same turn.
        assert_eq!(Turn::after(since, true, true, later), since);
        assert_eq!(
            Turn::after(since, true, false, later),
            Some(Turn::Took(Duration::from_secs(90)))
        );
        // At rest, the last turn stays.
        assert_eq!(Turn::after(took, false, false, later), took);
        assert_eq!(Turn::after(None, false, false, later), None);
    }

    #[test]
    fn gives_durations_in_two_units() {
        let of = |seconds| duration(Duration::from_secs(seconds));
        assert_eq!(of(0), "0s");
        assert_eq!(of(59), "59s");
        assert_eq!(of(60), "1m 00s");
        assert_eq!(of(192), "3m 12s");
        assert_eq!(of(3599), "59m 59s");
        assert_eq!(of(3840), "1h 04m");
    }

    #[test]
    fn keeps_the_shell_within_its_limits() {
        // As dragged, within its limits.
        assert_eq!(shell_share(px(300.), px(1000.)), 0.3);
        assert_eq!(shell_share(px(10.), px(1000.)), 0.096);
        assert_eq!(shell_share(px(990.), px(1000.)), MAX_SHELL_SHARE);
        // Drawn the share of the window it takes, never shorter than its
        // header and a few lines.
        assert_eq!(shell_height(0.3, px(1000.)), px(300.));
        assert_eq!(shell_height(0.3, px(200.)), MIN_SHELL_HEIGHT);
        assert_eq!(shell_height(0.99, px(1000.)), px(750.));
    }

    fn message(id: &str, parent: Option<&str>, role: &str, text: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type": role, "uuid": id, "parentUuid": parent,
                "message": { "role": role, "content": [{ "type": "text", "text": text }] }
            })
        )
    }

    #[test]
    fn shutdown_imports_unread_turns_and_sessions_in_queued_reports() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agentshub.db");
        let db = Db::open(&path).unwrap();
        let project = db.add_project(&dir).unwrap();
        let chat = db.add_chat(project, Agent::Claude, None).unwrap();
        let first = dir.join("first.jsonl");
        let second = dir.join("second.jsonl");
        let prompt = message("1", None, "user", "first prompt");
        fs::write(&first, &prompt).unwrap();
        let session = db
            .record_session(chat, Agent::Claude, "first", &first, Some("startup"))
            .unwrap()
            .session_id;
        transcript::read(&db, session).unwrap();
        // No status report triggers a read of the last answer.
        fs::write(
            &first,
            format!(
                "{prompt}{}",
                message("2", Some("1"), "assistant", "last answer")
            ),
        )
        .unwrap();
        fs::write(&second, message("3", None, "user", "second prompt")).unwrap();
        let (mut reports, _) = Reports::new();
        // This session switch is still queued when the workspace is released.
        fs::write(
            reports.path("cli"),
            format!(
                "{}\n",
                json!({
                    "session_id": "second", "transcript_path": second, "source": "clear"
                })
            ),
        )
        .unwrap();

        let store = Store::new(db.clone()).unwrap();
        import_chat(&store, &mut reports, chat, Agent::Claude, "cli", Vec::new()).unwrap();
        assert!(reports.read("cli", Agent::Claude).is_empty());
        reports.remove();
        // The original transcripts can expire; the database still has both.
        fs::remove_file(&first).unwrap();
        fs::remove_file(&second).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let sessions = db.sessions(chat).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|session| session.prompted));
        assert_eq!(
            db.items(sessions[0].id, 100).unwrap()[1].payload["text"],
            "last answer"
        );
        assert_eq!(
            db.items(sessions[1].id, 100).unwrap()[0].payload["text"],
            "second prompt"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// Restarting a chat kills its CLI and opens the chat again on its last
    /// recorded session. A report the CLI left queued says it had gone into
    /// another one, so it has to be recorded before the CLI goes, or the
    /// restart resumes a session the CLI had already left and the one it was
    /// really in is never recorded.
    #[test]
    fn restarting_records_the_session_the_cli_reported() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = Db::open_in_memory().unwrap();
        let project = db.add_project(&dir).unwrap();
        let chat = db.add_chat(project, Agent::Claude, None).unwrap();
        let first = dir.join("first.jsonl");
        let second = dir.join("second.jsonl");
        fs::write(&first, message("1", None, "user", "first prompt")).unwrap();
        fs::write(&second, message("2", None, "user", "second prompt")).unwrap();
        db.record_session(chat, Agent::Claude, "first", &first, Some("startup"))
            .unwrap();
        let (mut reports, _) = Reports::new();
        // The CLI reported the switch, and the report is still queued.
        fs::write(
            reports.path("cli"),
            format!(
                "{}\n",
                json!({
                    "session_id": "second", "transcript_path": second, "source": "clear"
                })
            ),
        )
        .unwrap();
        let last = |db: &Db| db.sessions(chat).unwrap().pop().unwrap().agent_id;
        assert_eq!(last(&db), "first");

        let queued = reported_sessions(&mut reports, "cli", Agent::Claude);
        assert_eq!(
            queued,
            vec![(
                "second".to_string(),
                second.clone(),
                Some("clear".to_string())
            )]
        );
        for (agent_id, path, reason) in queued {
            db.record_session(chat, Agent::Claude, &agent_id, &path, reason.as_deref())
                .unwrap();
        }
        // What opening the chat again resumes.
        assert_eq!(last(&db), "second");
        // The reports are taken out, so stopping leaves nothing behind.
        assert!(reported_sessions(&mut reports, "cli", Agent::Claude).is_empty());

        reports.remove();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shutdown_continues_importing_after_one_transcript_fails() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = Db::open_in_memory().unwrap();
        let project = db.add_project(&dir).unwrap();
        let chat = db.add_chat(project, Agent::Claude, None).unwrap();
        // A directory cannot be read as a transcript.
        let bad = db
            .record_session(chat, Agent::Claude, "bad", &dir, None)
            .unwrap()
            .session_id;
        let good = dir.join("good.jsonl");
        fs::write(&good, message("1", None, "user", "kept")).unwrap();
        let session = db
            .record_session(chat, Agent::Claude, "good", &good, None)
            .unwrap()
            .session_id;
        let (mut reports, _) = Reports::new();

        let sessions = vec![bad, session];
        let store = Store::new(db.clone()).unwrap();
        assert!(import_chat(&store, &mut reports, chat, Agent::Claude, "cli", sessions).is_err());
        assert_eq!(db.items(session, 100).unwrap()[0].payload["text"], "kept");
        reports.remove();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pending_worktree_removal_is_done_once() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let root = dir.join("gone");
        let worktree = Worktree {
            root: root.clone(),
            folder: root,
        };
        let mut lifecycle = Lifecycle::default();
        let job = lifecycle.track_removal(&worktree, &dir, Vec::new());
        // Whichever of its task and shutdown comes first removes it.
        assert_eq!(take_and_remove(&job).unwrap().unwrap(), Removal::Removed);
        assert!(take_and_remove(&job).is_none());
        assert!(
            lifecycle
                .take_removals()
                .iter()
                .all(|job| take_and_remove(job).is_none())
        );
        fs::remove_dir_all(dir).ok();
    }
}
