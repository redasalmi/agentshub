use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::base::{ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::resizable::{HandleEdge, resize_handle, resize_handle_appearance};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _, Theme,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::{RecursiveMode, Watcher as _};

use crate::agent::Agent;
use crate::alerts::{self, Alert};
use crate::appearance;
use crate::db::{self, Db};
use crate::history::History;
use crate::palette::{self, Entry, Lead, Pick};
use crate::reports::{self, Reports};
use crate::sessions;
use crate::settings::{
    Appearance, DEFAULT_RAIL_WIDTH, DEFAULT_SHELL_SHARE, DEFAULT_SIDEBAR_WIDTH, Settings,
    ThemeFamily,
};
use crate::sidebar::{self, Filter, NUMBERED, Sidebar, SidebarEvent, Tone, home_relative};
use crate::status::Status;
use crate::terminal::{self, Exited, Pty, StatusChanged, Terminal};
use crate::terminal_view::TerminalView;
use crate::transcript;
use crate::worktree::{self, Worktree};

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

/// The most of the window's height that a chat's earlier sessions take.
const HISTORY_SHARE: f32 = 0.45;

/// The most of the window's height the shell can be dragged to take.
const MAX_SHELL_SHARE: f32 = 0.75;

/// The least height a chat's CLI keeps when the earlier sessions and the
/// shell crowd it. They give up their height to it, in proportion to it.
const MIN_TERMINAL_HEIGHT: Pixels = px(160.);

/// The least height the shell keeps: its header and a few lines.
const MIN_SHELL_HEIGHT: Pixels = px(96.);

/// Codex writes its session files in bursts, so they're looked at once it
/// pauses.
const CODEX_DELAY: Duration = Duration::from_millis(500);

/// Said above a chat whose last session couldn't be resumed, and kept in that
/// session's history.
const TRANSCRIPT_GONE: &str = "This session's transcript is gone, so it couldn't be resumed. \
                               Its saved history is still available here.";

actions!(
    workspace,
    [
        ToggleShell,
        OpenSearch,
        NewChat,
        AddProject,
        ToggleSettings,
        ToggleHistory,
        RenameChat,
        TogglePin,
        DeleteChat,
        ArchiveChat,
        ArchiveProject,
        ShowArchived,
        CloseAllChats,
        CloseIdleChats,
        ToggleTheme,
        ExportDatabase,
        ImportDatabase,
        ToggleProjectNames,
        ShowRunning,
        NextNeedingYou,
    ]
);

/// Puts the app in a family of themes.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct UseTheme(pub ThemeFamily);

/// Opens the `n`th chat of the sidebar's list, counting from zero.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct OpenChatAt(pub usize);

/// Selects the `n`th project of the sidebar's rail, counting from zero.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct SelectProjectAt(pub usize);

// The shortcuts are bound without a context, so they reach the workspace
// before a focused terminal sends them to its CLI. They hold Ctrl and Shift
// together, or Alt or Ctrl with a digit, which terminal programs leave alone,
// so the CLIs keep the plain Ctrl keys.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-k", OpenSearch, None),
        KeyBinding::new("ctrl-shift-n", NewChat, None),
        KeyBinding::new("ctrl-`", ToggleShell, None),
        KeyBinding::new("ctrl-shift-h", ToggleHistory, None),
        KeyBinding::new("f2", RenameChat, None),
        KeyBinding::new("ctrl-shift-o", AddProject, None),
        KeyBinding::new("ctrl-,", ToggleSettings, None),
        KeyBinding::new("ctrl-shift-w", CloseAllChats, None),
        KeyBinding::new("ctrl-shift-i", CloseIdleChats, None),
        KeyBinding::new("ctrl-shift-a", ArchiveChat, None),
        KeyBinding::new("ctrl-0", ShowRunning, None),
        KeyBinding::new("ctrl-shift-j", NextNeedingYou, None),
    ]);
    for n in 0..NUMBERED {
        let digit = n + 1;
        cx.bind_keys([
            KeyBinding::new(&format!("alt-{digit}"), OpenChatAt(n), None),
            KeyBinding::new(&format!("ctrl-{digit}"), SelectProjectAt(n), None),
        ]);
    }
}

impl SelectItem for Agent {
    type Value = Agent;

    fn title(&self) -> SharedString {
        self.display_name().into()
    }

    fn value(&self) -> &Self::Value {
        self
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex().gap_2().child(self.display_name()).child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().muted_foreground)
                .child(self.program()),
        )
    }
}

pub struct Workspace {
    sidebar: Entity<Sidebar>,
    db: Db,
    /// Open chats, including exited terminals and history whose CLI couldn't
    /// start. Running processes keep running while another chat is shown,
    /// until the user closes them.
    chats: Vec<Chat>,
    /// The chat that is shown.
    active: Option<i64>,
    /// The chat the user last asked to show. Opening a chat reads its last
    /// session before starting its CLI, so a request can finish after a newer
    /// one; only the latest one may take the view.
    requested: Option<i64>,
    /// How many times the user has asked to show something: a chat, or the
    /// settings, or the chat again in their place. Something that finishes
    /// later than it started, like a chat's worktree being checked out, only
    /// takes the view if this hasn't changed meanwhile.
    navigations: u64,
    /// The chats whose last session is being read before their CLI starts,
    /// with whether it still should once the read is done. Closing all the
    /// chats cancels them, and opening one again revives its read.
    opening: HashMap<i64, bool>,
    /// The chats whose CLI was stopped to free what it held, until it has been
    /// reaped, with the session it was in. Until then the CLI may still write
    /// to that session, so nothing else may run in it.
    stopping: HashMap<i64, (smol::channel::Receiver<()>, Option<i64>)>,
    /// The last Codex session lookup started. Each lookup marks the chats it
    /// looked at with this, so an older lookup that finishes later can be
    /// told apart from a newer one.
    codex_lookup: u64,
    reports: Reports,
    codex_watcher: Option<notify::RecommendedWatcher>,
    /// What the watcher looks at: Codex's session folder, or while it doesn't
    /// exist yet, the closest folder above it.
    codex_watch: Option<(PathBuf, RecursiveMode)>,
    _sidebar_events: Subscription,
    _report_events: Task<()>,
    _codex_events: Task<()>,
    _theme: Subscription,
    _focus_lost: Subscription,
    /// What the open new chat dialog listens to: its agent and its title.
    new_chat_subscriptions: Vec<Subscription>,
    /// Why the workspace can't be used: the database didn't open, or another
    /// Agents Hub holds the lock.
    broken: Option<SharedString>,
    /// The sessions whose transcript is being read, which shutdown reads
    /// itself since it can't wait for them. A session is in it once per read
    /// under way, so it stays until the last of them is done.
    reading: Vec<i64>,
    /// The worktrees being checked out for new chats. Each is taken by its
    /// chat once git is done, or by shutdown, so none is left without a chat
    /// to remove it.
    checkouts: Vec<worktree::Checkout>,
    /// How many times all the chats were closed. A worktree checked out for
    /// a new chat before then doesn't start its chat, and goes again.
    closed_all: u64,
    /// The worktrees of deleted chats waiting for their CLI and shells to be
    /// reaped. Each is removed by its task, or by shutdown if the app quits
    /// first, so none is left once its chat is gone.
    removals: Vec<PendingRemoval>,
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

/// A worktree and its project's folder, until someone takes them to remove
/// the worktree. The remover holds the lock while git runs, so shutdown
/// waits for a removal under way rather than quit midway.
type RemovalJob = Arc<Mutex<Option<(Worktree, PathBuf)>>>;

/// A worktree to remove once what ran in it has been reaped.
struct PendingRemoval {
    job: RemovalJob,
    /// The CLI and the shells that ran in it, until they're reaped.
    waits: Vec<smol::channel::Receiver<()>>,
}

impl PendingRemoval {
    /// Whether it still has to be removed, or is being removed.
    fn is_pending(&self) -> bool {
        self.job.try_lock().map_or(true, |job| job.is_some())
    }
}

/// Takes the worktree out of `job` and removes it, unless it was taken.
fn take_and_remove(
    job: &Mutex<Option<(Worktree, PathBuf)>>,
) -> Option<anyhow::Result<worktree::Removal>> {
    let mut job = job.lock().unwrap_or_else(PoisonError::into_inner);
    let (worktree, project) = job.take()?;
    Some(worktree::remove(&worktree, &project))
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
    /// What the CLI last reported doing, if it reports that at all.
    status: Option<Status>,
    /// Whether the CLI finished while another chat was shown, and hasn't been
    /// looked at since.
    unseen: bool,
    /// The Codex session lookup that last looked at this chat. A lookup that
    /// started earlier must not record its result after a newer one, or it
    /// would move the chat back to a session it has left.
    codex_lookup: u64,
    /// Whether that lookup is still running.
    codex_pending: bool,
    /// Whether the user quit the CLI, so the chat closes once that lookup has
    /// recorded what it found.
    closing: bool,
    /// Something about the session to show above the terminal, without a
    /// restart button.
    notice: Option<SharedString>,
    /// Whether restarting starts a new session instead of resuming the last.
    fresh_on_restart: bool,
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
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (db, lock, imported, broken) = match db::open_locked() {
            Ok((db, lock, imported)) => (db, lock, imported, None),
            Err(err) => {
                eprintln!("{err:#}");
                (
                    Db::open_in_memory().expect("SQLite opens in memory"),
                    None,
                    None,
                    Some(format!("{err:#}").into()),
                )
            }
        };
        // Said once the window is up, since its notifications aren't yet.
        if let Some(imported) = imported {
            let message = match imported {
                db::Imported::Replaced(Some(backup)) => format!(
                    "Imported the database. The one it replaced is backed up at {}",
                    home_relative(&backup)
                ),
                db::Imported::Replaced(None) => "Imported the database.".into(),
                db::Imported::Failed(err) => {
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

        cx.set_global(Settings::load(&db));
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
                SidebarEvent::PinChat(id, pinned) => {
                    if let Err(err) = this.db.set_pinned(*id, *pinned) {
                        eprintln!("can't pin chat {id}: {err}");
                    }
                    this.refresh(cx);
                }
                SidebarEvent::MoveChat(id, to) => {
                    if let Err(err) = this.db.move_chat(*id, *to) {
                        eprintln!("can't move chat {id}: {err}");
                    }
                    this.refresh(cx);
                }
                SidebarEvent::MoveProject(id, to) => {
                    if let Err(err) = this.db.move_project(*id, *to) {
                        eprintln!("can't move project {id}: {err}");
                    }
                    this.refresh(cx);
                }
                SidebarEvent::CloseChat(id) => this.confirm_close_chat(*id, window, cx),
                SidebarEvent::DeleteChat(id) => this.confirm_delete_chat(*id, window, cx),
                SidebarEvent::ArchiveChat(id) => this.confirm_archive_chat(*id, window, cx),
                SidebarEvent::RestoreChat(id) => {
                    if let Err(err) = this.db.restore_chat(*id) {
                        eprintln!("can't restore chat {id}: {err}");
                    }
                    this.refresh(cx);
                }
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

        let (codex_tx, codex_changed) = smol::channel::unbounded();
        let codex_watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    return;
                };
                // Look at the files only when they are written. Opening a rollout
                // to find its session is an access, and scanning on that would
                // schedule the next scan forever.
                if !matches!(
                    event.kind,
                    notify::EventKind::Create(_) | notify::EventKind::Modify(_)
                ) {
                    return;
                }
                codex_tx.try_send(()).ok();
            })
            .inspect_err(|err| eprintln!("can't watch Codex's sessions: {err}"))
            .ok();
        let codex_events = cx.spawn(async move |this, cx| {
            while codex_changed.recv().await.is_ok() {
                cx.background_executor().timer(CODEX_DELAY).await;
                while codex_changed.try_recv().is_ok() {}
                let found = this.update(cx, |this, cx| {
                    this.update_codex_watch();
                    this.find_codex_sessions(cx);
                });
                if found.is_err() {
                    break;
                }
            }
        });

        // The workspace outlives its window (see `tray`), so it's released
        // when the app quits.
        cx.on_release(|this, cx| {
            // Nothing can be scheduled on a released workspace, so what the
            // CLIs wrote last is read here: the sessions in their queued
            // reports, the ones they were in, and the reads still under way.
            // The sessions they left before were read when they left them.
            let mut sessions = this.reading.clone();
            sessions.extend(this.stopping.values().filter_map(|(_, session)| *session));
            // A chat being opened reads its last session on a thread of its
            // own, which isn't in `reading`, so it is read again here, the
            // way `open_chat` reads it.
            for &id in this.opening.keys() {
                if let Some(last) = this.sessions_of(id).pop()
                    && last.transcript_path.exists()
                {
                    sessions.push(last.id);
                }
            }
            // Discover Codex's last session before its process disappears.
            // A queued watcher callback cannot run once we're released.
            for chat in &this.chats {
                if chat.agent == Agent::Codex
                    && let Some(terminal) = &chat.terminal
                    && let Some(path) = terminal
                        .terminal
                        .read(cx)
                        .with_pid(sessions::codex_session_of)
                    && let Some(agent_id) = transcript::session_id(chat.agent, &path)
                {
                    match this
                        .db
                        .record_session(chat.id, chat.agent, &agent_id, &path, None)
                    {
                        Ok(recorded) => {
                            sessions.extend(recorded.left.into_iter().chain([recorded.session_id]))
                        }
                        Err(err) => eprintln!("can't record the last Codex session: {err}"),
                    }
                }
            }
            // The CLIs are killed on threads of their own, which the app would
            // otherwise exit before they are done with.
            let reaped: Vec<_> = this
                .chats
                .iter()
                .filter_map(|chat| chat.terminal.as_ref())
                .map(|terminal| terminal.terminal.read(cx).shutdown())
                .chain(this.stopping.values().map(|(reaped, _)| reaped.clone()))
                .chain(
                    this.shells
                        .values()
                        .map(|shell| shell.terminal.read(cx).shutdown()),
                )
                .chain(
                    this.removals
                        .iter()
                        .flat_map(|removal| removal.waits.iter().cloned()),
                )
                .collect();
            terminal::wait_reaped(reaped);
            worktree::abandon(std::mem::take(&mut this.checkouts));
            // The worktrees of deleted chats, whose tasks won't run anymore.
            // One being removed already is waited for.
            for removal in std::mem::take(&mut this.removals) {
                if let Some(Err(err)) = take_and_remove(&removal.job) {
                    eprintln!("can't remove a worktree on shutdown: {err:#}");
                }
            }
            for chat in &this.chats {
                if let Err(err) = import_chat(
                    &this.db,
                    &mut this.reports,
                    chat.id,
                    chat.agent,
                    &chat.report,
                    chat.session.into_iter().collect(),
                ) {
                    eprintln!("can't import chat {} on shutdown: {err}", chat.id);
                }
            }
            sessions.sort_unstable();
            sessions.dedup();
            for id in sessions {
                if let Err(err) = transcript::read(&this.db, id) {
                    eprintln!("can't read session {id} on shutdown: {err}");
                }
            }
            this.reports.remove();
        })
        .detach();

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
            db,
            chats: Vec::new(),
            active: None,
            requested: None,
            navigations: 0,
            opening: HashMap::new(),
            stopping: HashMap::new(),
            codex_lookup: 0,
            reports,
            codex_watcher,
            codex_watch: None,
            _sidebar_events: sidebar_events,
            _report_events: report_events,
            _codex_events: codex_events,
            _theme: theme,
            _focus_lost: focus_lost,
            new_chat_subscriptions: Vec::new(),
            broken,
            reading: Vec::new(),
            checkouts: Vec::new(),
            closed_all: 0,
            removals: Vec::new(),
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
        this.refresh(cx);
        this
    }

    fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a folder: {err}"), cx)
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.add_folders(&[path], window, cx))
                .ok();
        })
        .detach();
    }

    /// Adds the folders among paths dropped on the sidebar as projects, and
    /// says so when none of them is a folder.
    fn add_dropped(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let folders: Vec<PathBuf> = paths.iter().filter(|path| path.is_dir()).cloned().collect();
        if folders.is_empty() {
            window.push_notification("Drop a folder to add it as a project", cx);
            return;
        }
        self.add_folders(&folders, window, cx);
    }

    /// Adds folders as projects, in the order given at the top of the list,
    /// and selects the first. A folder that already is a project is selected
    /// where it is.
    fn add_folders(&mut self, folders: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let mut first = None;
        // Each one goes on top, so the last goes in first.
        for path in folders.iter().rev() {
            let path = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            match self.db.add_project(&path) {
                Ok(id) => first = Some(id),
                Err(err) => {
                    window.push_notification(format!("Couldn't add {}: {err}", path.display()), cx)
                }
            }
        }
        let Some(id) = first else {
            return;
        };
        self.refresh(cx);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select(id, cx));
    }

    /// Copies the database where the user asks, so the history can live
    /// outside the one file agentshub reads.
    fn export_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let name = format!("agentshub-{}.db", jiff::Zoned::now().date());
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a file: {err}"), cx)
                    })
                    .ok();
                    return;
                }
                _ => return,
            };
            this.update_in(cx, |this, window, cx| match this.db.export(&path) {
                Ok(()) => window
                    .push_notification(format!("Exported the database to {}", path.display()), cx),
                Err(err) => {
                    window.push_notification(format!("Couldn't export the database: {err}"), cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// Replaces the database with one the user picks, once it is checked and
    /// they have seen what it holds. Agents Hub restarts to use it, which
    /// stops the agents as quitting does, and backs up the database it
    /// replaces on the way back up.
    fn import_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // One pick and check at a time, so a second click doesn't open a
        // second picker, or a second dialog over the first.
        if self.importing {
            return;
        }
        self.importing = true;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a file: {err}"), cx)
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            let Some(path) = path else {
                this.update(cx, |this, _| this.importing = false).ok();
                return;
            };
            // Checking and copying a large database takes a while.
            let prepared = smol::unblock({
                let path = path.clone();
                move || db::prepare_import(&path)
            })
            .await;
            // Another import can start once this one is shown: the dialog is
            // modal, and each check has a copy of its own. The window may have
            // closed to the tray meanwhile, which the workspace outlives, so
            // this doesn't wait for it.
            this.update(cx, |this, _| this.importing = false).ok();
            let unshown = prepared.as_ref().ok().map(|(pending, _)| pending.clone());
            let shown = this.update_in(cx, |this, window, cx| match prepared {
                Ok((pending, projects)) => {
                    this.confirm_import(&path, pending, projects, window, cx)
                }
                Err(err) => window.push_notification(
                    format!("Couldn't import {}: {err:#}", home_relative(&path)),
                    cx,
                ),
            });
            // Without a window to confirm it in, the copy is turned down.
            if shown.is_err()
                && let Some(pending) = unshown
            {
                pending.discard();
            }
        })
        .detach();
    }

    /// Shows what a checked database holds, and restarts with it in place of
    /// the database once the user confirms.
    fn confirm_import(
        &mut self,
        path: &Path,
        pending: db::PendingImport,
        projects: Vec<db::Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let chats: usize = projects.iter().map(|project| project.chats.len()).sum();
        let holds = match (projects.len(), chats) {
            (0, _) => format!("{name} has no projects."),
            (projects, chats) => format!(
                "{name} has {} and {}:",
                plural(projects, "project"),
                plural(chats, "chat")
            ),
        };
        let dir = db::path()
            .and_then(|path| path.parent().map(home_relative))
            .unwrap_or_default();
        let projects: Rc<[db::Project]> = projects.into();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let theme = cx.theme();
            let (confirm, discard) = (pending.clone(), pending.clone());
            alert
                .confirm()
                .width(px(520.))
                .title("Replace your chats with these?")
                .description(holds.clone())
                .when(!projects.is_empty(), |alert| {
                    alert.child(render_import(&projects, theme))
                })
                .child(
                    div()
                        .text_sm()
                        .line_height(rems(1.4))
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "Agents Hub restarts to use them, which stops the agents running \
                             now. Your projects, chats and settings are replaced; the database \
                             holding them is backed up first in {dir}, and importing that \
                             backup brings them back."
                        )),
                )
                .ok_text("Import and restart")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    match confirm.confirm() {
                        Ok(()) => cx.restart(),
                        Err(err) => window.push_notification(
                            format!("Couldn't import the database: {err:#}"),
                            cx,
                        ),
                    }
                    true
                })
                // The checked copy is gone once confirmed, so this only
                // deletes one the user turned down.
                .on_close(move |_, _, _| discard.discard())
        });
    }

    fn confirm_remove_project(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let has_worktrees = self.db.projects().is_ok_and(|projects| {
            projects
                .iter()
                .filter(|project| project.id == id)
                .flat_map(|project| &project.chats)
                .any(|chat| chat.worktree.is_some())
        });
        let description = if has_worktrees {
            "Its agents are stopped, and its chats are deleted from Agents Hub. \
             The folder and the agents' own session files stay. The worktrees of its chats \
             are removed unless they have changes nobody committed, and their branches stay."
        } else {
            "Its agents are stopped, and its chats are deleted from Agents Hub. \
             The folder and the agents' own session files stay."
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Remove this project?")
                .description(description)
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| {
                        let projects = this.db.projects().unwrap_or_default();
                        let project = projects.iter().find(|project| project.id == id);
                        let mut worktrees = Vec::new();
                        for chat in project.into_iter().flat_map(|project| &project.chats) {
                            let reaped = this
                                .stop_chat(chat.id, cx)
                                .or_else(|| this.reaping(chat.id));
                            if let Some(worktree) = &chat.worktree {
                                worktrees.push((worktree.clone(), reaped));
                            }
                        }
                        // Dropping its terminal kills the shell.
                        if let Some(project) = project {
                            this.shells.remove(&project.path);
                            this.sync_terminals(cx);
                        }
                        match this.db.remove_project(id) {
                            // Their notifications can't open them anymore.
                            Ok(()) => {
                                for chat in project.into_iter().flat_map(|project| &project.chats) {
                                    alerts::withdraw(chat.id, cx);
                                }
                                if let Some(project) = project {
                                    for (worktree, reaped) in worktrees {
                                        this.remove_worktree(
                                            &worktree,
                                            &project.path,
                                            reaped,
                                            window,
                                            cx,
                                        );
                                    }
                                }
                            }
                            Err(err) => eprintln!("can't remove project {id}: {err}"),
                        }
                        this.refresh(cx);
                    })
                    .ok();
                    true
                })
        });
    }

    /// Closes a running chat, asking first if its agent is in the middle of
    /// something. Opening the chat again resumes its session.
    fn confirm_close_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let working = self
            .chats
            .iter()
            .any(|chat| chat.id == id && chat.status == Some(Status::Working));
        if !working {
            self.close_chats(vec![id], window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Close this chat?")
                .description(
                    "Its agent is working, and closing the chat stops it mid-turn. \
                     Opening the chat again resumes its session.",
                )
                .ok_text("Close")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.close_chats(vec![id], window, cx))
                        .ok();
                    true
                })
        });
    }

    fn confirm_delete_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let worktree = self.worktree_of(id);
        let description = match worktree {
            Some(_) => {
                "Its agent is stopped, and the chat is deleted from Agents Hub. \
                 The agent's own session files stay. Its worktree is removed unless it has \
                 changes nobody committed, and its branch stays."
            }
            None => {
                "Its agent is stopped, and the chat is deleted from Agents Hub. \
                 The agent's own session files stay."
            }
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, worktree) = (this.clone(), worktree.clone());
            alert
                .confirm()
                .title("Delete this chat?")
                .description(description)
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| {
                        let reaped = this.stop_chat(id, cx).or_else(|| this.reaping(id));
                        match this.db.delete_chat(id) {
                            // Its notification can't open it anymore.
                            Ok(()) => {
                                alerts::withdraw(id, cx);
                                if let Some((worktree, project)) = &worktree {
                                    this.remove_worktree(worktree, project, reaped, window, cx);
                                }
                            }
                            Err(err) => eprintln!("can't delete chat {id}: {err}"),
                        }
                        this.refresh(cx);
                    })
                    .ok();
                    true
                })
        });
    }

    /// Archives a chat, closing it first, and asks first if its agent is in
    /// the middle of something.
    fn confirm_archive_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let working = self
            .chats
            .iter()
            .any(|chat| chat.id == id && chat.status == Some(Status::Working));
        if !working {
            self.archive_chat(id, window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Archive this chat?")
                .description(
                    "Its agent is working, and archiving the chat stops it mid-turn. \
                     Opening the chat again resumes its session.",
                )
                .ok_text("Archive")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.archive_chat(id, window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Closes a chat and archives it. Opening it again brings it back and
    /// resumes its session.
    fn archive_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_opening(id);
        self.close_chats(vec![id], window, cx);
        if let Err(err) = self.db.set_chat_archived(id, true) {
            window.push_notification(format!("Couldn't archive the chat: {err}"), cx);
            return;
        }
        alerts::withdraw(id, cx);
        self.refresh(cx);
        self.offer_undo(
            "Archived the chat.",
            move |db| db.set_chat_archived(id, false),
            window,
            cx,
        );
    }

    /// Archives a project, closing its chats, and asks first if any of their
    /// agents is in the middle of something.
    fn confirm_archive_project(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.chats_of(id, cx);
        let working = self
            .chats
            .iter()
            .any(|chat| ids.contains(&chat.id) && chat.status == Some(Status::Working));
        if !working {
            self.archive_project(id, window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Archive this project?")
                .description(
                    "An agent in it is working, and archiving the project stops it mid-turn. \
                     Opening one of its chats again brings the project back and resumes the \
                     chat's session.",
                )
                .ok_text("Archive")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.archive_project(id, window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Closes a project's chats and its shell, and archives it with them.
    /// Opening one of its chats again brings it back.
    fn archive_project(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, worktrees)) = self
            .sidebar
            .read(cx)
            .projects()
            .iter()
            .find(|project| project.id == id)
            .map(|project| {
                let worktrees: Vec<PathBuf> = project
                    .chats
                    .iter()
                    .filter_map(|chat| Some(chat.worktree.as_ref()?.root.clone()))
                    .collect();
                (project.path.clone(), worktrees)
            })
        else {
            return;
        };
        let ids = self.chats_of(id, cx);
        for id in &ids {
            self.cancel_opening(*id);
        }
        self.close_chats(ids.clone(), window, cx);
        // Dropping their terminals kills the shells, as removing the project
        // does: its own, and the ones in its chats' worktrees.
        let shells = self.shells.len();
        self.shells
            .retain(|cwd, _| *cwd != path && !worktrees.iter().any(|root| cwd.starts_with(root)));
        if self.shells.len() != shells {
            self.sync_terminals(cx);
        }
        if let Err(err) = self.db.set_project_archived(id, true) {
            window.push_notification(format!("Couldn't archive the project: {err}"), cx);
            return;
        }
        for id in ids {
            alerts::withdraw(id, cx);
        }
        self.refresh(cx);
        self.offer_undo(
            "Archived the project.",
            move |db| db.set_project_archived(id, false),
            window,
            cx,
        );
    }

    /// The ids of a project's chats.
    fn chats_of(&self, project_id: i64, cx: &App) -> Vec<i64> {
        self.sidebar
            .read(cx)
            .projects()
            .iter()
            .filter(|project| project.id == project_id)
            .flat_map(|project| &project.chats)
            .map(|chat| chat.id)
            .collect()
    }

    /// Keeps a chat whose last session is being read, or whose stopped CLI
    /// is being reaped, from starting once that's done.
    fn cancel_opening(&mut self, id: i64) {
        if let Some(start) = self.opening.get_mut(&id) {
            *start = false;
        }
        if self.requested == Some(id) {
            self.requested = None;
        }
    }

    /// Says `message` with a button that runs `undo` on the database.
    fn offer_undo(
        &self,
        message: &'static str,
        undo: impl Fn(&Db) -> rusqlite::Result<()> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let undo = Rc::new(undo);
        window.push_notification(
            Notification::new()
                .message(message)
                .action(move |_, _, cx| {
                    let (this, undo) = (this.clone(), undo.clone());
                    Button::new("undo").label("Undo").on_click(cx.listener(
                        move |note, _, window, cx| {
                            this.update(cx, |this, cx| {
                                if let Err(err) = undo(&this.db) {
                                    eprintln!("can't undo: {err}");
                                }
                                this.refresh(cx);
                            })
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

    /// What says a chat's CLI that was stopped to free what it held has
    /// been reaped, while it hasn't yet.
    fn reaping(&self, id: i64) -> Option<smol::channel::Receiver<()>> {
        self.stopping
            .get(&id)
            .map(|(reaped, _)| reaped.clone())
            .filter(|reaped| !reaped.is_closed())
    }

    fn open_rename_dialog(
        &mut self,
        id: i64,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(title.clone())
                .placeholder("Its agent's title")
        });
        let this = cx.entity().downgrade();
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                input.focus_handle(cx).focus(window, cx);
                input.update(cx, |input, cx| input.select_all(window, cx));
            }
            let (this, input, title) = (this.clone(), input.clone(), title.clone());
            dialog
                .title("Rename this chat")
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Leave it empty to title it after its agent's sessions."),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("rename").primary().label("Rename")),
                        ),
                )
                // Enter in the input confirms too.
                .on_ok(move |_, _, cx| {
                    let value = input.read(cx).value();
                    // An untouched title stays the agent's, and follows it.
                    if value != title {
                        this.update(cx, |this, cx| {
                            if let Err(err) = this.db.rename_chat(id, &value) {
                                eprintln!("can't rename chat {id}: {err}");
                            }
                            this.refresh(cx);
                        })
                        .ok();
                    }
                    true
                })
        });
    }

    fn open_new_chat_dialog(
        &mut self,
        project_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = match self.db.projects() {
            Ok(projects) => projects
                .into_iter()
                .find(|project| project.id == project_id),
            Err(err) => {
                eprintln!("can't list the projects: {err}");
                None
            }
        };
        let Some(project) = project else {
            return;
        };
        // Only the agents whose CLI is installed can start. Chats of the others
        // stay as they are.
        let agents = Agent::installed(&project.path);
        let none_installed = agents.is_empty();
        // A chat can have a worktree of its own when the project is in a git
        // repository. Whether it does starts as the settings say.
        let in_git = !none_installed && worktree::repo(&project.path).is_some();
        let use_worktree = Rc::new(Cell::new(
            in_git && cx.global::<Settings>().worktree_by_default,
        ));
        let select = cx.new(|cx| SelectState::new(agents, None, window, cx));
        // Left empty, the chat is titled after its agent's sessions.
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Chat name (optional)"));
        let project_label = SharedString::from(project.path.to_string_lossy().into_owned());
        self.new_chat_subscriptions = vec![
            cx.subscribe_in(&select, window, {
                let (use_worktree, title) = (use_worktree.clone(), title.clone());
                move |this, _, event, window, cx| {
                    if let SelectEvent::Confirm(Some(agent)) = event {
                        let title = title.read(cx).value().to_string();
                        window.close_dialog(cx);
                        let worktree = use_worktree.get();
                        this.new_chat(
                            project_id,
                            *agent,
                            &project.path,
                            worktree,
                            title,
                            window,
                            cx,
                        );
                    }
                }
            }),
            // Choosing the agent starts the chat, so Enter in the title goes
            // on to it.
            cx.subscribe_in(&title, window, {
                let select = select.clone();
                move |_, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        select.focus_handle(cx).focus(window, cx);
                    }
                }
            }),
        ];

        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) && !none_installed {
                select.focus_handle(cx).focus(window, cx);
            }
            dialog
                .title("Start a coding agent")
                // Only choosing an agent starts the chat. Enter in the title
                // goes on to it rather than confirming, and so closing, the
                // dialog.
                .on_ok(|_, _, _| false)
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(project_label.clone()),
                        )
                        .map(|content| {
                            if none_installed {
                                content.child(div().text_sm().child(
                                    "No coding agent is installed. Install Claude Code, \
                                 Codex or Pi, with its command on your PATH.",
                                ))
                            } else {
                                // Above the agent, whose choice starts the chat.
                                content
                                    .child(Input::new(&title))
                                    .child(Select::new(&select).placeholder("Choose an agent"))
                            }
                        })
                        .when(in_git, |content| {
                            let use_worktree = use_worktree.clone();
                            content.child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        Checkbox::new("new-chat-worktree")
                                            .label("Start in a new git worktree")
                                            .checked(use_worktree.get())
                                            .on_click(move |checked, window, _| {
                                                use_worktree.set(*checked);
                                                window.refresh();
                                            }),
                                    )
                                    .child(
                                        div()
                                            .pl_6()
                                            .text_sm()
                                            .line_height(rems(1.4))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                "The agent works on a branch of its own, in a \
                                             checkout of the project that other chats don't \
                                             touch.",
                                            ),
                                    ),
                            )
                        }),
                )
        });
    }

    /// Starts an agent in a new chat of a project, in a new worktree of its
    /// own when `worktree` is set. An empty `title` leaves the chat titled
    /// after its agent's sessions.
    #[allow(clippy::too_many_arguments)]
    fn new_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        project: &Path,
        worktree: bool,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !worktree {
            self.add_chat(project_id, agent, project, None, &title, true, window, cx);
            return;
        }
        // Checking out the project takes a while in a large repository, and
        // the user may show another chat or the settings meanwhile. Then the
        // new chat starts in the background, so it can't take the keyboard.
        // Asking for it is itself a request to show something, so it
        // supersedes the chats still opening and the worktrees still being
        // checked out for earlier requests, which start in the background.
        self.navigations += 1;
        self.requested = None;
        let navigations = self.navigations;
        let closed_all = self.closed_all;
        let project = project.to_path_buf();
        let checkout = worktree::Checkout::start(&project);
        let created = checkout.created();
        self.checkouts.retain(|checkout| !checkout.is_taken());
        self.checkouts.push(checkout);
        cx.spawn_in(window, async move |this, cx| {
            // Shutdown takes it instead if the app quits first. It can't
            // between here and the chat being added, which is one step.
            let Ok(created) = created.recv().await else {
                return;
            };
            let added = this.update_in(cx, |this, window, cx| match &created {
                // All the chats were closed meanwhile, this one with them.
                Ok(worktree) if this.closed_all != closed_all => {
                    this.remove_worktree(worktree, &project, None, window, cx)
                }
                Err(_) if this.closed_all != closed_all => {}
                Ok(worktree) => {
                    // The project may have been removed meanwhile, and its id
                    // given to another project, which the chat isn't about.
                    match this.db.project_path(project_id) {
                        Ok(Some(path)) if path == project => {
                            let show = this.navigations == navigations;
                            this.add_chat(
                                project_id,
                                agent,
                                &project,
                                Some(worktree),
                                &title,
                                show,
                                window,
                                cx,
                            )
                        }
                        Ok(_) => this.remove_worktree(worktree, &project, None, window, cx),
                        Err(err) => {
                            window.push_notification(format!("Couldn't add the chat: {err}"), cx);
                            this.remove_worktree(worktree, &project, None, window, cx);
                        }
                    }
                }
                Err(err) => {
                    window.push_notification(format!("Couldn't create a worktree: {err:#}"), cx)
                }
            });
            // Without a window to start it in, no chat runs in the worktree.
            // The workspace outlives its window (see `tray`), so shutdown
            // still has to know of it.
            if added.is_err()
                && let Ok(worktree) = created
            {
                let app: &mut AsyncApp = cx;
                let job = this
                    .update(app, |this, _| {
                        this.track_removal(&worktree, &project, Vec::new())
                    })
                    .unwrap_or_else(|_| Arc::new(Mutex::new(Some((worktree, project)))));
                if let Some(Err(err)) = smol::unblock(move || take_and_remove(&job)).await {
                    eprintln!("can't remove a worktree without a window: {err:#}");
                }
            }
        })
        .detach();
    }

    /// Adds a chat and starts its agent in `project`, or in the `worktree`
    /// made for it, which goes again if the chat can't start. The chat is
    /// titled `title`, unless it's empty, and shown if `show` is set, and
    /// otherwise runs in the background.
    #[allow(clippy::too_many_arguments)]
    fn add_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        project: &Path,
        worktree: Option<&Worktree>,
        title: &str,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = worktree.map_or(project, |worktree| &worktree.folder);
        let id = match self.db.add_chat(project_id, agent, worktree) {
            Ok(id) => id,
            Err(err) => {
                window.push_notification(format!("Couldn't add the chat: {err}"), cx);
                if let Some(worktree) = worktree {
                    self.remove_worktree(worktree, project, None, window, cx);
                }
                return;
            }
        };
        if !title.trim().is_empty()
            && let Err(err) = self.db.rename_chat(id, title)
        {
            eprintln!("can't title chat {id}: {err}");
        }
        // The project may have been archived while the chat's worktree was
        // checked out. Starting the chat brings it back, as opening one does.
        if let Err(err) = self.db.restore_project(project_id) {
            eprintln!("can't restore project {project_id}: {err}");
        }
        // Its first session report adds its first session.
        if !self.start_chat(id, agent, cwd, None, window, cx) {
            let reaped = self.stop_chat(id, cx);
            self.db.delete_chat(id).ok();
            if let Some(worktree) = worktree {
                self.remove_worktree(worktree, project, reaped, window, cx);
            }
        } else if show {
            self.activate(id, window, cx);
            // The sidebar doesn't list the new chat yet, so it can't find
            // its project by itself.
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.select(project_id, cx));
        } else {
            // As a chat opened in the background does, it only shows in the
            // sidebar as running.
            self.sync_sidebar(cx);
            cx.notify();
        }
        self.refresh(cx);
    }

    /// The worktree Agents Hub made for chat `id`, and its project's folder.
    fn worktree_of(&self, id: i64) -> Option<(Worktree, PathBuf)> {
        let projects = self
            .db
            .projects()
            .inspect_err(|err| eprintln!("can't list the projects: {err}"))
            .ok()?;
        projects.into_iter().find_map(|project| {
            let chat = project.chats.into_iter().find(|chat| chat.id == id)?;
            Some((chat.worktree?, project.path))
        })
    }

    /// Has shutdown remove `worktree` once `waits` are reaped, unless the
    /// job it returns is taken first.
    fn track_removal(
        &mut self,
        worktree: &Worktree,
        project: &Path,
        waits: Vec<smol::channel::Receiver<()>>,
    ) -> RemovalJob {
        let job = Arc::new(Mutex::new(Some((worktree.clone(), project.to_path_buf()))));
        self.removals.retain(PendingRemoval::is_pending);
        self.removals.push(PendingRemoval {
            job: job.clone(),
            waits,
        });
        job
    }

    /// Removes the worktree a chat ran in once its CLI, if it had one, and
    /// the shells running in it have been reaped. Git keeps one that has
    /// changes nobody committed, which the user is told about.
    fn remove_worktree(
        &mut self,
        worktree: &Worktree,
        project: &Path,
        reaped: Option<smol::channel::Receiver<()>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inside: Vec<PathBuf> = self
            .shells
            .keys()
            .filter(|cwd| cwd.starts_with(&worktree.root))
            .cloned()
            .collect();
        let mut waits: Vec<_> = reaped.into_iter().collect();
        for cwd in &inside {
            if let Some(shell) = self.shells.remove(cwd) {
                waits.push(shell.terminal.read(cx).shutdown());
            }
        }
        if !inside.is_empty() {
            self.sync_terminals(cx);
            cx.notify();
        }
        let folder = worktree.root.clone();
        let job = self.track_removal(worktree, project, waits.clone());
        cx.spawn_in(window, async move |this, cx| {
            // What the CLI and the shells write as they exit, like a shell's
            // history, would otherwise go to a folder being deleted.
            for reaped in waits {
                reaped.recv().await.ok();
            }
            // Shutdown removed it if the app quit first.
            let Some(removed) = smol::unblock(move || take_and_remove(&job)).await else {
                return;
            };
            let message = match removed {
                Ok(worktree::Removal::Removed) => return,
                Ok(worktree::Removal::Rescued(branch)) => format!(
                    "Removed the worktree at {}. Commits made there on a detached HEAD are \
                     kept on the branch {branch}.",
                    home_relative(&folder)
                ),
                Ok(worktree::Removal::Kept(why)) => format!(
                    "Kept the worktree at {}: {}",
                    home_relative(&folder),
                    why.trim_start_matches("fatal: ")
                ),
                Err(err) => {
                    eprintln!("can't remove the worktree {}: {err:#}", folder.display());
                    format!(
                        "Couldn't remove the worktree at {}: {err:#}",
                        home_relative(&folder)
                    )
                }
            };
            this.update_in(cx, |_, window, cx| window.push_notification(message, cx))
                .ok();
        })
        .detach();
    }

    /// Shows a chat, resuming its last session if its CLI doesn't run.
    /// Shows a chat, starting its CLI unless it runs. An archived chat, or
    /// one in an archived project, is brought back first.
    pub fn open_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_archived(id, cx) {
            if let Err(err) = self.db.restore_chat(id) {
                eprintln!("can't restore chat {id}: {err}");
            }
            self.refresh(cx);
        }
        self.navigations += 1;
        if self.chats.iter().any(|chat| chat.id == id) {
            // Reading a chat's last session takes long enough for another
            // chat to be asked for meanwhile. This is the request that a
            // completion compares itself with.
            self.requested = Some(id);
            self.activate(id, window, cx);
            return;
        }
        let (agent, cwd) = match self.db.chat(id) {
            Ok(Some(chat)) => chat,
            Ok(None) => return,
            Err(err) => {
                eprintln!("can't open chat {id}: {err}");
                return;
            }
        };
        // A CLI that was stopped to free what it held opens again once it's
        // gone, so the two never run in one session.
        if let Some((reaped, _)) = self
            .stopping
            .get(&id)
            .filter(|(reaped, _)| !reaped.is_closed())
        {
            let reaped = reaped.clone();
            self.requested = Some(id);
            cx.spawn_in(window, async move |this, cx| {
                reaped.recv().await.ok();
                this.update_in(cx, |this, window, cx| {
                    if this.requested == Some(id) {
                        this.open_chat(id, window, cx);
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        // The chat exists, so this request is now the one that decides what
        // the view shows when the read below finishes.
        self.requested = Some(id);
        // Clicks that arrive while the session is read would start a second
        // CLI in the same chat. A read that was cancelled starts it after all.
        if let Some(start) = self.opening.get_mut(&id) {
            *start = true;
            return;
        }
        self.opening.insert(id, true);

        // The last session decides how the CLI starts, so it's read first: the
        // CLI may have gone on without agentshub. Transcripts can be large
        // enough that this can't happen while the window waits.
        let db = self.db.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = smol::unblock(move || -> anyhow::Result<()> {
                let mut sessions = db.sessions(id)?;
                if let Some(last) = sessions.pop()
                    && last.transcript_path.exists()
                {
                    transcript::read(&db, last.id)?;
                    // Opening a chat is the chat being active, which orders the
                    // sidebar. Reading its history must not do the same.
                    db.touch_session(last.id)?;
                }
                // The history hides a session without a prompt, so it is never
                // read again there, even once a newer parser would find one.
                for earlier in sessions
                    .iter()
                    .filter(|session| !session.prompted && session.transcript_path.exists())
                {
                    if let Err(err) = transcript::read(&db, earlier.id) {
                        eprintln!("can't read session {}: {err}", earlier.id);
                    }
                }
                Ok(())
            })
            .await;
            if let Err(err) = read {
                eprintln!("can't read the last session of chat {id}: {err}");
            }
            if this
                .update_in(cx, |this, window, cx| {
                    // The chats may have been closed meanwhile.
                    if this.opening.remove(&id) != Some(true) {
                        return;
                    }
                    // The chat may have been deleted, or opened elsewhere, meanwhile.
                    if this.chats.iter().any(|chat| chat.id == id) {
                        return;
                    }
                    match this.db.chat(id) {
                        Ok(Some(_)) => {}
                        Ok(None) => return,
                        Err(err) => {
                            eprintln!("can't open chat {id}: {err}");
                            return;
                        }
                    }
                    let sessions = this.sessions_of(id);
                    let mut notice = None;
                    let resumed = match sessions.last() {
                        Some(last) if last.prompted && last.transcript_path.exists() => Some(last),
                        // Claude deletes old transcripts. What the chat kept of the
                        // session stays in its history, and the banner says why a
                        // new session started.
                        Some(last) if last.prompted => {
                            match this.db.set_missing(last.id) {
                                Ok(true) => {
                                    this.db.add_notice(last.id, TRANSCRIPT_GONE).ok();
                                }
                                Ok(false) => {}
                                Err(err) => {
                                    eprintln!("can't mark session {} missing: {err}", last.id)
                                }
                            }
                            notice = Some(TRANSCRIPT_GONE.into());
                            None
                        }
                        // A session without a prompt has nothing to resume.
                        _ => None,
                    };
                    // A chat the user selected while this one was being read
                    // keeps the view. This one still starts, but in the
                    // background, so its completion can't steal focus.
                    let show = this.requested == Some(id);
                    // Two CLIs in one session would append to the same transcript,
                    // so a session another chat is running can't be resumed here.
                    if let Some(last) = resumed
                        && this.live_elsewhere(id, last.id)
                    {
                        this.block_chat(
                            id,
                            agent,
                            &cwd,
                            "This session is running in another chat. Start a new session to \
                         keep going here."
                                .into(),
                            cx,
                        );
                    } else if this.start_chat(id, agent, &cwd, resumed, window, cx)
                        && let Some(notice) = notice
                        && let Some(chat) = this.chats.iter_mut().find(|chat| chat.id == id)
                    {
                        chat.notice = Some(notice);
                    }
                    if show {
                        this.activate(id, window, cx);
                    } else {
                        this.sync_sidebar(cx);
                        cx.notify();
                    }
                })
                .is_err()
            {
                // The window closed meanwhile, so the chat isn't started, but
                // it can be opened again.
                this.update(cx, |this, _| this.opening.remove(&id)).ok();
            }
        })
        .detach();
    }

    fn sessions_of(&self, chat_id: i64) -> Vec<db::Session> {
        self.db.sessions(chat_id).unwrap_or_else(|err| {
            eprintln!("can't list the sessions of chat {chat_id}: {err}");
            Vec::new()
        })
    }

    fn read_reports(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some((id, agent)) = self
            .chats
            .iter()
            .find(|chat| chat.report == key)
            .map(|chat| (chat.id, chat.agent))
        else {
            return;
        };
        let mut recorded = false;
        for (agent_id, report) in self.reports.read(key, agent) {
            recorded |= self.record_session(
                id,
                &agent_id,
                &report.transcript_path,
                report.reason.as_deref(),
                cx,
            );
        }
        // Reports arrive in bursts, and each one used to reload the sidebar.
        if recorded {
            self.refresh(cx);
        }
    }

    /// Codex writes its session files as it goes, so the sessions of its
    /// terminals are looked for whenever those change.
    fn find_codex_sessions(&mut self, cx: &mut Context<Self>) {
        // Each chat's report name identifies this terminal, not just its row:
        // A chat can restart its terminal while the lookup runs.
        //
        // A lookup can take longer than the watcher's delay, so a newer one
        // can finish first. Marking the chats a lookup looks at with its
        // generation lets a result that has been superseded be dropped rather
        // than move a chat back to the session it has already left.
        let lookup = self.codex_lookup.wrapping_add(1);
        let mut chats = Vec::new();
        for chat in &mut self.chats {
            if chat.agent != Agent::Codex {
                continue;
            }
            let Some(terminal) = &chat.terminal else {
                continue;
            };
            let Some(pid) = terminal.terminal.read(cx).with_pid(Some) else {
                continue;
            };
            chat.codex_lookup = lookup;
            chat.codex_pending = true;
            chats.push((chat.id, chat.report.clone(), pid));
        }
        if chats.is_empty() {
            return;
        }
        self.codex_lookup = lookup;
        cx.spawn(async move |this, cx| {
            // Looking through a process's open files, and reading the first
            // line of the newest one, doesn't belong on the window's thread.
            let found = smol::unblock(move || {
                chats
                    .into_iter()
                    .filter_map(|(id, report, pid)| {
                        let path = sessions::codex_session_of(pid)?;
                        let agent_id = transcript::session_id(Agent::Codex, &path)?;
                        Some((id, report, path, agent_id))
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            let Ok(closing) = this.update(cx, |this, cx| {
                let mut recorded = false;
                for (id, report, path, agent_id) in found {
                    // A chat with this id now is another chat, or the same
                    // one with a newer terminal: either way the result is
                    // about a process that has gone. The same goes for a
                    // lookup that a newer one has already replaced.
                    let current = this
                        .chats
                        .iter()
                        .find(|chat| chat.id == id && chat.report == report)
                        .is_some_and(|chat| {
                            chat.codex_lookup == lookup && chat.transcript.as_ref() != Some(&path)
                        });
                    if !current {
                        continue;
                    }
                    recorded |= this.record_session(id, &agent_id, &path, None, cx);
                }
                if recorded {
                    this.refresh(cx);
                }
                let mut closing = Vec::new();
                for chat in &mut this.chats {
                    if chat.codex_pending && chat.codex_lookup == lookup {
                        chat.codex_pending = false;
                        if chat.closing {
                            closing.push(chat.id);
                        }
                    }
                }
                closing
            }) else {
                return;
            };
            if closing.is_empty() {
                return;
            }
            // One closed while its CLI was exiting may not be reaped yet, so
            // its session is read once it is, and nothing else runs in that
            // session until then. The shown chat takes the keyboard with it,
            // which only its window can give back.
            let Ok(window) = this.read_with(cx, |this, _| this.window) else {
                return;
            };
            let closed = window.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| this.close_chats(closing.clone(), window, cx))
            });
            // Closed to the tray, the window has no keyboard to give back, and
            // opening it again focuses the shown chat.
            if !matches!(closed, Ok(Ok(()))) {
                this.update(cx, |this, cx| {
                    for id in closing {
                        this.suspend(id, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn update_codex_watch(&mut self) {
        let (Some(watcher), Some(dir)) = (&mut self.codex_watcher, sessions::codex_sessions_dir())
        else {
            return;
        };
        let wanted = if dir.is_dir() {
            Some((dir, RecursiveMode::Recursive))
        } else {
            dir.ancestors()
                .skip(1)
                .find(|dir| dir.is_dir())
                .map(|dir| (dir.to_path_buf(), RecursiveMode::NonRecursive))
        };
        if wanted == self.codex_watch {
            return;
        }
        if let Some((path, _)) = self.codex_watch.take() {
            watcher.unwatch(&path).ok();
        }
        if let Some((path, mode)) = wanted {
            match watcher.watch(&path, mode) {
                Ok(()) => self.codex_watch = Some((path, mode)),
                Err(err) => eprintln!("can't watch {}: {err}", path.display()),
            }
        }
    }

    /// Records the session a chat's CLI went into, and reads the one it left.
    /// Says whether it recorded anything.
    fn record_session(
        &mut self,
        chat_id: i64,
        agent_id: &str,
        path: &Path,
        reason: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.chats.iter().position(|chat| chat.id == chat_id) else {
            return false;
        };
        let (id, agent) = (self.chats[index].id, self.chats[index].agent);
        self.chats[index].transcript = Some(path.to_path_buf());
        match self.db.record_session(id, agent, agent_id, path, reason) {
            Ok(recorded) => {
                self.chats[index].session = Some(recorded.session_id);
                if let Some(left) = recorded.left {
                    self.read_sessions(vec![left], false, cx).detach();
                }
                true
            }
            Err(err) => {
                eprintln!("can't record session {agent_id}: {err}");
                false
            }
        }
    }

    /// Reads sessions' transcripts on a blocking thread, then shows what
    /// changed. Marks their chats active when `touch` is set, as a finished
    /// turn does but browsing history doesn't.
    fn read_sessions(
        &mut self,
        sessions: Vec<i64>,
        touch: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let db = self.db.clone();
        self.reading.extend(&sessions);
        cx.spawn(async move |this, cx| {
            if sessions.is_empty() {
                return;
            }
            // Parsing transcripts and writing them to the database blocks, so
            // it stays off the executor's threads.
            let (sessions, changed) = smol::unblock(move || {
                let mut changed = Vec::new();
                for &id in &sessions {
                    match transcript::read(&db, id) {
                        Ok(read) => {
                            if touch {
                                db.touch_session(id).ok();
                            }
                            if read {
                                changed.push(id);
                            }
                        }
                        Err(err) => {
                            eprintln!("can't read session {id}: {err}");
                        }
                    }
                }
                (sessions, changed)
            })
            .await;
            this.update(cx, |this, cx| {
                for id in &sessions {
                    if let Some(index) = this.reading.iter().position(|reading| reading == id) {
                        this.reading.swap_remove(index);
                    }
                }
                this.refresh_after(&changed, cx);
            })
            .ok();
        })
    }

    /// Lists the projects and chats again, and the earlier sessions of the
    /// chats whose CLI runs.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_after(&[], cx);
    }

    /// The same, reloading the expanded sessions whose transcript read, in
    /// `changed`, found something.
    fn refresh_after(&mut self, changed: &[i64], cx: &mut Context<Self>) {
        match self.db.projects() {
            Ok(projects) => self
                .sidebar
                .update(cx, |sidebar, cx| sidebar.set_projects(projects, cx)),
            Err(err) => eprintln!("can't list the projects: {err}"),
        }
        for chat in &self.chats {
            let current = chat.current_session();
            chat.history
                .update(cx, |history, cx| history.refresh(current, changed, cx));
        }
    }

    fn set_status(&mut self, id: i64, status: Option<Status>, cx: &mut Context<Self>) {
        let shown = self.active == Some(id);
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) else {
            return;
        };
        let busy = |status| matches!(status, Some(Status::Working | Status::Blocked(_)));
        let was_busy = busy(chat.status);
        let alert = Alert::of(chat.agent, chat.status, status);
        let agent = chat.agent;
        chat.unseen = match status {
            Some(Status::Done | Status::Error) => !shown,
            // Codex goes back to idle without saying it's done. The others
            // say so, and go back to idle when a turn is cancelled.
            Some(Status::Idle) => chat.unseen || (chat.agent == Agent::Codex && was_busy && !shown),
            _ => false,
        };
        chat.status = status;
        // The session is read after each turn, so the chat's title follows it.
        if was_busy
            && !busy(status)
            && let Some(session) = chat.session
        {
            self.read_sessions(vec![session], true, cx).detach();
        }
        match (alert, status) {
            // Nobody needs telling about the chat they're looking at.
            (Some(alert), Some(status)) if !self.in_view(id, cx.active_window().is_some()) => {
                let body = self
                    .sidebar
                    .read(cx)
                    .describe(id)
                    .unwrap_or_else(|| "A chat in Agents Hub".into());
                alerts::alert(id, agent, alert, status, body, cx);
            }
            // Once the agent is back at work, what it needed has been seen to.
            (_, Some(Status::Working)) => alerts::withdraw(id, cx),
            _ => {}
        }
        self.sync_sidebar(cx);
    }

    /// Whether the user can see chat `id`: it's shown, nothing covers it and
    /// the window has the focus.
    fn in_view(&self, id: i64, focused: bool) -> bool {
        self.active == Some(id) && !self.settings_shown && !self.hidden && focused
    }

    /// Takes back the notification about the shown chat once the user can
    /// see it, as when the window takes the focus or the settings close.
    /// `focused` is passed in because, on X11, the window's activation is
    /// reported before `cx.active_window()` catches up with it.
    pub fn withdraw_seen(&self, focused: bool, cx: &mut App) {
        if let Some(id) = self.active
            && self.in_view(id, focused)
        {
            alerts::withdraw(id, cx);
        }
    }

    /// Marks the chat that is shown and what each running one is doing.
    fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        let running = self
            .chats
            .iter()
            .filter(|chat| chat.running())
            .map(|chat| {
                // The sidebar shows a finished CLI as done until it is seen.
                let status = match chat.status {
                    Some(Status::Idle | Status::Done) if chat.unseen => Some(Status::Done),
                    Some(Status::Done) => Some(Status::Idle),
                    status => status,
                };
                (chat.id, status)
            })
            .collect();
        let active = self.active;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_chats(running, active, cx));
    }

    /// Starts a chat's CLI, resuming `session` if given. The chat stays hidden
    /// until it is activated.
    fn start_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        session: Option<&db::Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let report = uuid::Uuid::new_v4().to_string();
        let colors = appearance::terminal_colors(cx);
        let mut all_args = self.reports.args(agent);
        if let Some(session) = session {
            all_args.extend(agent.resume_args(&session.agent_id, &session.transcript_path));
        }
        let report_path = self.reports.path(&report);
        let env = [(reports::ENV, report_path.as_os_str())];
        let terminal = match Pty::spawn(agent.program(), &all_args, &env, cwd, colors) {
            Ok(pty) => {
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
                let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
                // Hidden terminals don't need to build frames.
                terminal.read(cx).set_visible(false);
                let exited = cx.subscribe(
                    &terminal,
                    move |this, _, &Exited { success }: &Exited, cx| {
                        this.chat_exited(id, success, cx)
                    },
                );
                let status = cx.subscribe(
                    &terminal,
                    move |this, _, StatusChanged(status): &StatusChanged, cx| {
                        this.set_status(id, *status, cx)
                    },
                );
                Ok(ChatTerminal {
                    terminal,
                    view,
                    exited: false,
                    _exited: exited,
                    _status: status,
                })
            }
            Err(err) => {
                let message = format!("Couldn't start {}: {err}", agent.program());
                window.push_notification(message.clone(), cx);
                Err(SharedString::from(message))
            }
        };
        let started = terminal.is_ok();
        self.push_chat(id, agent, cwd, report, terminal, session, cx);
        started
    }

    /// Adds a chat to the view, with the history of the sessions it has: with
    /// its terminal, or with why its CLI didn't start.
    #[allow(clippy::too_many_arguments)]
    fn push_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        report: String,
        terminal: Result<ChatTerminal, SharedString>,
        session: Option<&db::Session>,
        cx: &mut Context<Self>,
    ) -> &mut Chat {
        let (terminal, message) = match terminal {
            Ok(terminal) => (Some(terminal), None),
            Err(message) => (None, Some(message)),
        };
        // Without a running CLI, even its last session belongs in history.
        let current = terminal.as_ref().and(session.map(|session| session.id));
        let transcript = session.map(|session| session.transcript_path.clone());
        let db = self.db.clone();
        let history = cx.new(|cx| {
            // Without a terminal, the history is all there is to show.
            let mut history = History::new(db, id, terminal.is_none());
            history.refresh(current, &[], cx);
            history
        });
        self.chats.push(Chat {
            id,
            agent,
            cwd: cwd.to_path_buf(),
            report,
            terminal,
            message,
            history,
            session: session.map(|session| session.id),
            transcript,
            status: None,
            unseen: false,
            codex_lookup: 0,
            codex_pending: false,
            closing: false,
            notice: None,
            fresh_on_restart: false,
        });
        self.chats.last_mut().expect("a chat was just pushed")
    }

    /// Whether another chat's CLI is running in a session.
    fn live_elsewhere(&self, chat_id: i64, session_id: i64) -> bool {
        self.chats
            .iter()
            .any(|chat| chat.id != chat_id && chat.current_session() == Some(session_id))
            || self.stopping.iter().any(|(&id, (reaped, session))| {
                id != chat_id && *session == Some(session_id) && !reaped.is_closed()
            })
    }

    /// Shows a chat whose CLI wasn't started, with the reason above the
    /// history. Restarting starts a new session rather than resuming the one
    /// that couldn't start.
    fn block_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        message: SharedString,
        cx: &mut Context<Self>,
    ) {
        let session = self
            .db
            .sessions(id)
            .ok()
            .and_then(|mut sessions| sessions.pop());
        let report = uuid::Uuid::new_v4().to_string();
        self.push_chat(id, agent, cwd, report, Err(message), session.as_ref(), cx)
            .fresh_on_restart = true;
        self.sync_sidebar(cx);
        cx.notify();
    }

    /// Restarts a chat's CLI, starting a new session when the chat couldn't
    /// have resumed its last one.
    fn restart(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .is_some_and(|chat| chat.fresh_on_restart);
        self.stop_chat(id, cx);
        if fresh {
            let Ok(Some((agent, cwd))) = self.db.chat(id) else {
                return;
            };
            if self.start_chat(id, agent, &cwd, None, window, cx) {
                self.activate(id, window, cx);
            }
            self.refresh(cx);
        } else {
            self.open_chat(id, window, cx);
        }
    }

    /// Archives the last session. A CLI the user quit closes its chat;
    /// otherwise the terminal's exit diagnostics stay until the user restarts
    /// or deletes the chat.
    fn chat_exited(&mut self, id: i64, success: bool, cx: &mut Context<Self>) {
        let Some(index) = self.chats.iter().position(|chat| chat.id == id) else {
            return;
        };
        // A report can still be queued when the CLI's exit is handled. It
        // says which session the final read follows, so it is read before the
        // last session is imported.
        let report = self.chats[index].report.clone();
        self.read_reports(&report, cx);
        let chat = &mut self.chats[index];
        // A CLI that quits by itself without failing, as on a startup error,
        // keeps its terminal too, since only typing quits it otherwise.
        let quit = success
            && chat.terminal.as_ref().is_some_and(|terminal| {
                terminal.terminal.read(cx).input() != terminal::Input::Untouched
            });
        if quit {
            // A running Codex lookup may still find the session the CLI was
            // in, and nothing else would record it.
            if chat.codex_pending {
                chat.closing = true;
            } else {
                self.close_chat(id, cx);
            }
            return;
        }
        if let Some(terminal) = &mut chat.terminal {
            terminal.exited = true;
        }
        chat.message = Some(
            format!(
                "{} exited. Its terminal output is kept below.",
                chat.agent.display_name()
            )
            .into(),
        );
        chat.status = None;
        let session = chat.session;
        chat.history
            .update(cx, |history, cx| history.refresh(None, &[], cx));
        self.sync_sidebar(cx);
        cx.notify();
        self.read_sessions(session.into_iter().collect(), false, cx)
            .detach();
    }

    /// Closes a chat whose CLI the user quit, and reads its last session.
    fn close_chat(&mut self, id: i64, cx: &mut Context<Self>) {
        let session = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(|chat| chat.session);
        self.stop_chat(id, cx);
        self.read_sessions(session.into_iter().collect(), false, cx)
            .detach();
    }

    /// Kills the chat's CLI, if it has one, and closes the chat. The channel
    /// closes once the CLI has been reaped.
    fn stop_chat(
        &mut self,
        id: i64,
        cx: &mut Context<Self>,
    ) -> Option<smol::channel::Receiver<()>> {
        let index = self.chats.iter().position(|chat| chat.id == id)?;
        let chat = self.chats.remove(index);
        let reaped = chat
            .terminal
            .map(|terminal| terminal.terminal.read(cx).shutdown());
        if self.active == Some(id) {
            self.active = None;
        }
        self.sync_terminals(cx);
        self.sync_sidebar(cx);
        cx.notify();
        reaped
    }

    /// Whether a chat's CLI can be stopped without losing anything the user
    /// would miss, shown or not: it's at rest, has nothing unseen, holds no
    /// draft, and can resume its session.
    fn at_rest(&self, chat: &Chat, cx: &App) -> bool {
        let last_prompt = || match chat.session {
            Some(session) => self.db.last_prompt(session),
            None => Ok((false, None)),
        };
        !chat.unseen
            && matches!(chat.status, Some(Status::Idle | Status::Done))
            && chat.terminal.as_ref().is_some_and(|terminal| {
                !terminal.exited
                    && terminal
                        .terminal
                        .read(cx)
                        .input()
                        .sent(|| last_prompt().ok().and_then(|(_, at)| at))
            })
            // Without its transcript, a session with a prompt can't be resumed,
            // and the CLI holds the only copy of it. Claude deletes transcripts
            // that went untouched for 30 days.
            && (chat.transcript.as_ref().is_some_and(|path| path.exists())
                || matches!(last_prompt(), Ok((false, _))))
    }

    /// Stops a chat's CLI after recording the session it's in, which opening
    /// the chat again resumes, and reads that session once the CLI is gone.
    fn suspend(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) else {
            return;
        };
        let report = chat.report.clone();
        // Codex doesn't report its sessions, so the one it's in is looked for
        // among its open files before it goes. That's a few reads under /proc,
        // which a chat stopping now and then can afford on this thread.
        let codex = chat
            .terminal
            .as_ref()
            .filter(|_| chat.agent == Agent::Codex)
            .and_then(|terminal| {
                terminal
                    .terminal
                    .read(cx)
                    .with_pid(sessions::codex_session_of)
            })
            .and_then(|path| Some((transcript::session_id(Agent::Codex, &path)?, path)));
        // A Codex that exited, or is exiting, while a lookup was under way
        // has no open files left to look through, so only that lookup can
        // find the session it was in. The chat stops once it has recorded it,
        // as when the user quits the CLI.
        if chat.codex_pending && codex.is_none() {
            chat.closing = true;
            return;
        }
        // A queued report can say the CLI went into another session.
        self.read_reports(&report, cx);
        if let Some((agent_id, path)) = codex {
            self.record_session(id, &agent_id, &path, None, cx);
        }
        let Some(session) = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .map(|chat| chat.session)
        else {
            return;
        };
        let Some(reaped) = self.stop_chat(id, cx) else {
            return;
        };
        self.stopping.insert(id, (reaped.clone(), session));
        cx.spawn(async move |this, cx| {
            // What the CLI writes as it exits is part of the session.
            reaped.recv().await.ok();
            this.update(cx, |this, cx| {
                // The chat may have been opened and stopped again since.
                if this
                    .stopping
                    .get(&id)
                    .is_some_and(|(stopping, _)| stopping.same_channel(&reaped))
                {
                    this.stopping.remove(&id);
                }
                this.read_sessions(session.into_iter().collect(), false, cx)
                    .detach();
            })
            .ok();
        })
        .detach();
    }

    fn activate(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) {
            chat.unseen = false;
        }
        alerts::withdraw(id, cx);
        self.active = Some(id);
        self.requested = Some(id);
        self.navigations += 1;
        self.settings_shown = false;
        self.focus_chat(window, cx);
        self.sync_terminals(cx);
        self.sync_sidebar(cx);
        // Showing a chat, even the one already shown, brings its project back
        // into the rail's selection, so the chat list and New Chat follow it.
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.reveal(id, cx));
        cx.notify();
    }

    fn active_cwd(&self) -> Option<&Path> {
        self.chats
            .iter()
            .find(|chat| Some(chat.id) == self.active)
            .map(|chat| chat.cwd.as_path())
    }

    /// The folder of the shell that shows: the shown chat's project, or
    /// without a chat, the selected project, or without one, the home folder.
    fn shell_cwd(&self, cx: &App) -> Option<PathBuf> {
        self.active_cwd()
            .map(Path::to_path_buf)
            .or_else(|| {
                self.sidebar
                    .read(cx)
                    .selected()
                    .map(|project| project.path.clone())
            })
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }

    fn shown_shell(&self, cx: &App) -> Option<(PathBuf, &Shell)> {
        let cwd = self.shell_cwd(cx)?;
        let shell = self.shells.get(&cwd)?;
        shell.shown.then_some((cwd, shell))
    }

    /// Whether the shell is on screen.
    fn shell_visible(&self, cx: &App) -> bool {
        !self.settings_shown && self.shown_shell(cx).is_some()
    }

    /// Without a chat shown, the shell is the selected project's, so it
    /// changes with the selection, and takes the focus from the one before.
    fn project_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_some() {
            return;
        }
        let shown = self
            .shown_shell(cx)
            .map(|(_, shell)| shell.view.focus_handle(cx));
        let in_shell = self
            .shells
            .values()
            .any(|shell| shell.view.focus_handle(cx).is_focused(window));
        if in_shell && !shown.as_ref().is_some_and(|focus| focus.is_focused(window)) {
            match shown {
                Some(focus) if !self.settings_shown => focus.focus(window, cx),
                _ => self.focus_chat(window, cx),
            }
        }
        self.sync_terminals(cx);
        cx.notify();
    }

    /// Only the shown chat's CLI and project shell build frames, and neither
    /// does while the settings cover them or the window is closed to the tray.
    fn sync_terminals(&self, cx: &App) {
        let shown = !self.hidden && !self.settings_shown;
        let chat = self.active.filter(|_| shown);
        for each in &self.chats {
            if let Some(terminal) = &each.terminal {
                terminal
                    .terminal
                    .read(cx)
                    .set_visible(Some(each.id) == chat);
            }
        }
        let shell = self.shown_shell(cx).map(|(cwd, _)| cwd).filter(|_| shown);
        for (cwd, each) in &self.shells {
            each.terminal
                .read(cx)
                .set_visible(Some(cwd) == shell.as_ref());
        }
    }

    /// Has the workspace shown in `window`, opened again from the tray.
    pub fn set_window(&mut self, window: &Window) {
        self.window = window.window_handle();
    }

    /// Stops the shown terminals from building frames while the window is
    /// closed to the tray, and starts them again when it opens.
    pub fn set_hidden(&mut self, hidden: bool, cx: &mut App) {
        self.hidden = hidden;
        self.sync_terminals(cx);
        self.withdraw_seen(cx.active_window().is_some(), cx);
    }

    /// Shows the settings in place of the chat, or the chat again. The focus
    /// leaves the covered terminals, so keys can't reach a CLI out of sight.
    pub fn show_settings(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_shown = shown;
        self.navigations += 1;
        if shown {
            // A chat still opening mustn't take the view from the settings.
            self.requested = None;
            self.focus_handle.focus(window, cx);
        } else {
            self.focus_chat(window, cx);
        }
        self.sync_terminals(cx);
        self.withdraw_seen(window.is_window_active(), cx);
        cx.notify();
    }

    /// Saves the theme's mode and puts the theme in it.
    fn set_appearance(&mut self, value: Appearance, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = Settings::set_appearance(&self.db, value, cx) {
            setting_not_saved(err, window, cx);
        }
        appearance::apply(window, cx);
        cx.notify();
    }

    /// Saves the family of themes and puts the theme in it.
    /// Names the projects in the sidebar's rail, or shows only their marks.
    fn show_project_names(&mut self, show: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = Settings::set_project_names(&self.db, show, cx) {
            setting_not_saved(err, window, cx);
        }
        self.sidebar.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn toggle_project_names(
        &mut self,
        _: &ToggleProjectNames,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let show = !cx.global::<Settings>().project_names;
        self.show_project_names(show, window, cx);
    }

    fn set_theme(&mut self, value: ThemeFamily, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = Settings::set_theme(&self.db, value, cx) {
            setting_not_saved(err, window, cx);
        }
        appearance::apply(window, cx);
        cx.notify();
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

    /// Shows the shell of the shown chat's project, or of the selected project
    /// without a chat, starting it if it isn't running, or hides it if it
    /// shows. Settings cover the shell, so they close and the shell shows.
    fn toggle_shell(&mut self, _: &ToggleShell, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.shell_cwd(cx) else {
            return;
        };
        let settings_were_shown = std::mem::take(&mut self.settings_shown);
        if !settings_were_shown && self.shells.get(&cwd).is_some_and(|shell| shell.shown) {
            if let Some(shell) = self.shells.get_mut(&cwd) {
                shell.shown = false;
            }
            self.focus_chat(window, cx);
        } else {
            if !self.shells.contains_key(&cwd) {
                let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let colors = appearance::terminal_colors(cx);
                let pty = match Pty::spawn(&program, &[], &[], &cwd, colors) {
                    Ok(pty) => pty,
                    Err(err) => {
                        // Nothing changes, so the settings stay up.
                        self.settings_shown = settings_were_shown;
                        window.push_notification(format!("Couldn't start {program}: {err}"), cx);
                        return;
                    }
                };
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
                let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
                let exited_cwd = cwd.clone();
                // Shells can exit while the window is closed to the tray.
                let exited = cx.subscribe(&terminal, move |this, _, _: &Exited, cx| {
                    this.shell_exited(&exited_cwd, cx)
                });
                self.shells.insert(
                    cwd.clone(),
                    Shell {
                        terminal,
                        view,
                        shown: true,
                        _exited: exited,
                    },
                );
            }
            let shell = self
                .shells
                .get_mut(&cwd)
                .expect("the shell was just started");
            // A shell of this project may be running hidden already.
            shell.shown = true;
            shell.view.focus_handle(cx).focus(window, cx);
        }
        self.sync_terminals(cx);
        // Closing the settings shows the chat, which no activation reports
        // while the window already has the focus.
        self.withdraw_seen(window.is_window_active(), cx);
        cx.notify();
    }

    /// Closes a shell that exited. The panel goes with it, and the keyboard
    /// moves back to the chat if the shell had it.
    fn shell_exited(&mut self, cwd: &Path, cx: &mut Context<Self>) {
        let Some(shell) = self.shells.remove(cwd) else {
            return;
        };
        if shell.shown {
            // Without a window there's no focus to move.
            cx.with_window(cx.entity_id(), |window, cx| {
                if shell.view.focus_handle(cx).is_focused(window) {
                    self.focus_chat(window, cx);
                }
            });
        }
        cx.notify();
    }
}

/// What the shortcuts, the search modal and the chat's menu run.
impl Workspace {
    /// The shown chat as the sidebar lists it.
    fn active_entry<'a>(&self, cx: &'a App) -> Option<&'a db::Chat> {
        let id = self.active?;
        self.sidebar
            .read(cx)
            .projects()
            .iter()
            .flat_map(|project| &project.chats)
            .find(|chat| chat.id == id)
    }

    /// Whether a chat, or its project, is archived.
    fn is_archived(&self, id: i64, cx: &App) -> bool {
        self.sidebar.read(cx).projects().iter().any(|project| {
            project.chats.iter().any(|chat| {
                chat.id == id && (chat.archived_at.is_some() || project.archived_at.is_some())
            })
        })
    }

    fn open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(false, window, cx);
    }

    fn show_archived(&mut self, _: &ShowArchived, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(true, window, cx);
    }

    /// Starts an agent in the selected project, or asks for a project when
    /// there's none.
    fn new_chat_here(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        match self.sidebar.read(cx).selected().map(|project| project.id) {
            Some(id) => self.open_new_chat_dialog(id, window, cx),
            None => self.add_project(window, cx),
        }
    }

    fn add_project_action(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        self.add_project(window, cx);
    }

    fn toggle_settings(&mut self, _: &ToggleSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings(!self.settings_shown, window, cx);
    }

    fn toggle_history(&mut self, _: &ToggleHistory, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.iter().find(|chat| Some(chat.id) == self.active) {
            chat.history.update(cx, |history, cx| history.toggle(cx));
            cx.notify();
        }
    }

    fn rename_active(&mut self, _: &RenameChat, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.active_entry(cx) {
            let (id, title) = (chat.id, chat.title.clone().unwrap_or_default());
            self.open_rename_dialog(id, title.into(), window, cx);
        }
    }

    fn toggle_pin(&mut self, _: &TogglePin, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.active_entry(cx) {
            let (id, pinned) = (chat.id, chat.pinned);
            if let Err(err) = self.db.set_pinned(id, !pinned) {
                eprintln!("can't pin chat {id}: {err}");
            }
            self.refresh(cx);
        }
    }

    fn delete_active(&mut self, _: &DeleteChat, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.confirm_delete_chat(id, window, cx);
        }
    }

    fn archive_active(&mut self, _: &ArchiveChat, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.confirm_archive_chat(id, window, cx);
        }
    }

    fn archive_selected(
        &mut self,
        _: &ArchiveProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.sidebar.read(cx).selected().map(|project| project.id) {
            self.confirm_archive_project(id, window, cx);
        }
    }

    /// Closes every open chat, stopping its CLI even mid-turn, and the ones
    /// still opening or waiting for their worktree. Opening a chat again
    /// resumes its session.
    fn close_all_chats(&mut self, _: &CloseAllChats, window: &mut Window, cx: &mut Context<Self>) {
        // A chat waiting for its last session to be read, or for its stopped
        // CLI to be reaped, would start once that's done.
        for start in self.opening.values_mut() {
            *start = false;
        }
        // So would a new chat once its worktree is checked out. The checkouts
        // stay listed, so shutdown still removes what they make if the app
        // quits first.
        self.closed_all += 1;
        for checkout in &self.checkouts {
            checkout.cancel();
        }
        self.requested = None;
        let ids = self.chats.iter().map(|chat| chat.id).collect();
        self.close_chats(ids, window, cx);
    }

    /// Closes the chats whose CLI sits at rest with nothing the user would
    /// miss, the shown one included. Opening one again resumes its session.
    fn close_idle_chats(
        &mut self,
        _: &CloseIdleChats,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self
            .chats
            .iter()
            .filter(|chat| self.at_rest(chat, cx))
            .map(|chat| chat.id)
            .collect();
        self.close_chats(ids, window, cx);
    }

    fn close_chats(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        // Settings in front of the chat keep the keyboard whatever closes,
        // as does the sidebar beside it.
        let chat_focused = self.active.is_some()
            && !self.settings_shown
            && self.chat_focus.contains_focused(window, cx);
        let focused_shell = self
            .shown_shell(cx)
            .filter(|(_, shell)| shell.view.focus_handle(cx).contains_focused(window, cx))
            .map(|(cwd, _)| cwd);
        for id in ids {
            self.suspend(id, cx);
        }
        let shell = self.shown_shell(cx);
        match focused_shell {
            // The shell under the shown chat follows the selected project once
            // the chat is gone, so a worktree's shell gives way to the
            // project's, which takes the keyboard if it's running.
            Some(cwd) if shell.as_ref().map(|(shown, _)| shown) != Some(&cwd) => match shell {
                Some((_, shell)) => shell.view.focus_handle(cx).focus(window, cx),
                None => self.focus_chat(window, cx),
            },
            // The shell still shows and keeps the keyboard.
            Some(_) => {}
            // The shown chat went, its terminal and header with it, and the
            // keyboard with them.
            None if chat_focused && self.active.is_none() => self.focus_chat(window, cx),
            None => {}
        }
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.set_appearance(appearance::other(cx), window, cx);
    }

    fn use_theme(&mut self, action: &UseTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.set_theme(action.0, window, cx);
    }

    fn export_action(&mut self, _: &ExportDatabase, window: &mut Window, cx: &mut Context<Self>) {
        self.export_database(window, cx);
    }

    fn import_action(&mut self, _: &ImportDatabase, window: &mut Window, cx: &mut Context<Self>) {
        self.import_database(window, cx);
    }

    fn open_chat_at(&mut self, action: &OpenChatAt, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.sidebar.read(cx).chat_at(action.0) {
            self.open_chat(id, window, cx);
        }
    }

    fn select_project_at(
        &mut self,
        action: &SelectProjectAt,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_at(action.0, cx));
    }

    fn show_running(&mut self, _: &ShowRunning, window: &mut Window, cx: &mut Context<Self>) {
        if self.running_menu.take().is_some() {
            cx.notify();
        } else {
            self.open_running_menu(window, cx);
        }
    }

    /// Opens the menu of the running chats of every project, under what
    /// their agents need, the most urgent first.
    fn open_running_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sidebar = self.sidebar.read(cx);
        let chats: Vec<_> = sidebar
            .running_chats()
            .into_iter()
            .map(|(project, chat, tone)| {
                let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                let about = sidebar::subtitle(chat, tone, Some(&project.name));
                (
                    chat.id,
                    SharedString::from(title),
                    SharedString::from(about),
                    tone,
                )
            })
            .collect();
        let active = self.active;
        let workspace = cx.weak_entity();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            menu = menu.min_w(px(300.)).max_w(px(400.)).scrollable(true);
            if chats.is_empty() {
                return menu.item(
                    PopupMenuItem::element(|_, cx| {
                        div()
                            .py_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No agents running")
                    })
                    .disabled(true),
                );
            }
            let mut heading = None;
            for (id, title, about, tone) in chats.iter().cloned() {
                let group = tone.group();
                if heading != Some(group) {
                    heading = Some(group);
                    let count = chats.iter().filter(|chat| chat.3.group() == group).count();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            sidebar::group_heading(group, count, tone, cx).into_any_element()
                        })
                        .disabled(true),
                    );
                }
                let workspace = workspace.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, cx| {
                        let theme = cx.theme();
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .py_1()
                            .gap(px(10.))
                            .child(sidebar::dot(tone, cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .truncate()
                                            .when(active == Some(id), |this| this.font_medium())
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(tone.text_color(cx))
                                            .truncate()
                                            .child(about.clone()),
                                    ),
                            )
                    })
                    .on_click(move |_, window, cx| {
                        workspace
                            .update(cx, |this, cx| this.open_chat(id, window, cx))
                            .ok();
                    }),
                );
            }
            menu
        });
        cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.running_menu = None;
            cx.notify();
        })
        .detach();
        self.running_menu = Some(menu);
        cx.notify();
    }

    /// Opens the next chat whose agent needs the user, going round them all
    /// when pressed again.
    fn next_needing_you(
        &mut self,
        _: &NextNeedingYou,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.sidebar.read(cx).next_needing_you(self.active) {
            Some(id) => self.open_chat(id, window, cx),
            None => window.push_notification("No agent needs you", cx),
        }
    }

    /// Opens the search modal over the chats of every project, the agents
    /// the selected project can start, the projects, the commands, and what
    /// is archived; or with `archived_only`, over what is archived alone.
    fn open_palette(&mut self, archived_only: bool, window: &mut Window, cx: &mut Context<Self>) {
        let sidebar = self.sidebar.read(cx);
        let mut chats = Vec::new();
        let mut archived = Vec::new();
        for project in sidebar.projects() {
            for chat in &project.chats {
                // An archived project's chats are archived along with it.
                if let Some(at) = chat.archived_at.or(project.archived_at) {
                    let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                    archived.push((
                        at,
                        Entry::new(title, Lead::Tone(Tone::Saved), Pick::OpenChat(chat.id))
                            .meta(format!(
                                "{} · archived {} · {}",
                                chat.agent.label(),
                                sidebar::age(at),
                                project.name
                            ))
                            .keywords([chat.agent.display_name(), "archived"]),
                    ));
                    continue;
                }
                let tone = Tone::of(sidebar.running(chat.id));
                let doing = match tone {
                    Tone::Saved => sidebar::age(chat.updated_at),
                    tone => tone.label().to_string(),
                };
                let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                chats.push((
                    tone.needs_you(),
                    Entry::new(title, Lead::Tone(tone), Pick::OpenChat(chat.id))
                        .meta(format!(
                            "{} · {doing} · {}",
                            chat.agent.label(),
                            project.name
                        ))
                        .keywords([chat.agent.display_name()]),
                ));
            }
        }
        // The chats that need the user come first.
        chats.sort_by_key(|(needs_you, _)| !needs_you);
        let chats = chats.into_iter().map(|(_, entry)| entry).collect();

        let selected = sidebar.selected();
        let start_heading = match selected {
            Some(project) => format!("Start an agent in {}", project.name),
            None => "Start an agent".into(),
        };
        let start = selected
            .map(|project| {
                Agent::installed(&project.path)
                    .into_iter()
                    .map(|agent| {
                        Entry::new(
                            agent.display_name(),
                            Lead::Icon(IconName::Plus.into()),
                            Pick::StartAgent(project.id, agent),
                        )
                        .meta(agent.program())
                        .keywords(["new chat", "start"])
                    })
                    .collect()
            })
            .unwrap_or_default();

        for project in sidebar.projects() {
            if let Some(at) = project.archived_at {
                archived.push((
                    at,
                    Entry::new(
                        project.name.clone(),
                        Lead::Icon(ExtraIcon::Archive),
                        Pick::SelectProject(project.id),
                    )
                    .meta(format!(
                        "archived {} · {}",
                        sidebar::age(at),
                        home_relative(&project.path)
                    ))
                    .keywords(["project", "archived"]),
                ));
            }
        }
        // The last archived first.
        archived.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        let archived: Vec<_> = archived.into_iter().map(|(_, entry)| entry).collect();

        let this = cx.entity().downgrade();
        let on_pick = move |pick, window: &mut Window, cx: &mut App| {
            this.update(cx, |this, cx| this.run_pick(pick, window, cx))
                .ok();
        };
        if archived_only {
            palette::open(vec![("Archived".into(), archived)], on_pick, window, cx);
            return;
        }

        let projects = sidebar
            .projects()
            .iter()
            .filter(|project| project.archived_at.is_none())
            .map(|project| {
                Entry::new(
                    project.name.clone(),
                    Lead::Icon(IconName::FolderOpen.into()),
                    Pick::SelectProject(project.id),
                )
                .meta(home_relative(&project.path))
                .keywords(["project"])
            })
            .collect();

        let run = |label: &str, icon: IconName, action: Box<dyn Action>| {
            Entry::new(
                label.to_string(),
                Lead::Icon(icon.into()),
                Pick::Run(action),
            )
        };
        let mut commands = Vec::new();
        if let Some(chat) = self.active_entry(cx) {
            commands.push(run(
                "Show or hide the earlier sessions",
                IconName::Inbox,
                Box::new(ToggleHistory),
            ));
            commands.push(run(
                "Show or hide the project's shell",
                IconName::SquareTerminal,
                Box::new(ToggleShell),
            ));
            commands.push(run(
                "Rename this chat",
                IconName::Replace,
                Box::new(RenameChat),
            ));
            commands.push(run(
                if chat.pinned {
                    "Unpin this chat"
                } else {
                    "Pin this chat to the top"
                },
                IconName::Star,
                Box::new(TogglePin),
            ));
            commands.push(
                Entry::new(
                    "Archive this chat",
                    Lead::Icon(ExtraIcon::Archive),
                    Pick::Run(Box::new(ArchiveChat)),
                )
                .keywords(["hide"]),
            );
            commands.push(run(
                "Delete this chat",
                IconName::Delete,
                Box::new(DeleteChat),
            ));
        }
        if selected.is_some() {
            commands.push(
                Entry::new(
                    "Archive this project",
                    Lead::Icon(ExtraIcon::Archive),
                    Pick::Run(Box::new(ArchiveProject)),
                )
                .keywords(["hide"]),
            );
        }
        if !archived.is_empty() {
            commands.push(Entry::new(
                "Show the archived chats and projects",
                Lead::Icon(ExtraIcon::ArchiveRestore),
                Pick::Run(Box::new(ShowArchived)),
            ));
        }
        if !self.chats.is_empty() {
            commands.push(
                run(
                    "Close the idle chats",
                    IconName::Close,
                    Box::new(CloseIdleChats),
                )
                .keywords(["stop"]),
            );
            commands.push(
                run(
                    "Close all the chats",
                    IconName::Close,
                    Box::new(CloseAllChats),
                )
                .keywords(["stop"]),
            );
        }
        let filter = sidebar.filter();
        for each in Filter::ALL.into_iter().filter(|each| *each != filter) {
            let label = match each {
                Filter::All => "Show all the chats",
                Filter::NeedsYou => "Show the chats that need you",
                Filter::Running => "Show the running chats",
            };
            commands.push(Entry::new(
                label,
                Lead::Icon(IconName::Search.into()),
                Pick::Filter(each),
            ));
        }
        commands.push(
            Entry::new(
                "Show the running chats of every project",
                Lead::Icon(ExtraIcon::Activity),
                Pick::Run(Box::new(ShowRunning)),
            )
            .keywords(["working", "idle", "active"]),
        );
        commands.push(
            Entry::new(
                "Open the next chat that needs you",
                Lead::Icon(ExtraIcon::CircleAlert),
                Pick::Run(Box::new(NextNeedingYou)),
            )
            .keywords(["waiting", "permission", "question", "done", "error"]),
        );
        commands.push(run(
            "Add a project",
            IconName::FolderOpen,
            Box::new(AddProject),
        ));
        commands.push(run(
            if cx.global::<Settings>().project_names {
                "Hide the projects' names"
            } else {
                "Show the projects' names"
            },
            if cx.global::<Settings>().project_names {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeftOpen
            },
            Box::new(ToggleProjectNames),
        ));
        commands.push(run(
            if cx.theme().is_dark() {
                "Use the light theme"
            } else {
                "Use the dark theme"
            },
            if cx.theme().is_dark() {
                IconName::Sun
            } else {
                IconName::Moon
            },
            Box::new(ToggleTheme),
        ));
        let family = cx.global::<Settings>().theme;
        for each in ThemeFamily::ALL.into_iter().filter(|each| *each != family) {
            commands.push(
                run(
                    &format!("Use the {} theme", each.label()),
                    IconName::Palette,
                    Box::new(UseTheme(each)),
                )
                .keywords(["theme", "colors"]),
            );
        }
        commands.push(run(
            "Settings",
            IconName::Settings,
            Box::new(ToggleSettings),
        ));
        commands.push(run(
            "Export a copy of the database",
            IconName::HardDrive,
            Box::new(ExportDatabase),
        ));
        commands.push(
            run(
                "Import a database",
                IconName::HardDrive,
                Box::new(ImportDatabase),
            )
            .keywords(["restore", "backup"]),
        );

        palette::open(
            vec![
                ("Chats".into(), chats),
                (start_heading.into(), start),
                ("Projects".into(), projects),
                ("Commands".into(), commands),
                ("Archived".into(), archived),
            ],
            on_pick,
            window,
            cx,
        );
    }

    fn run_pick(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            Pick::OpenChat(id) => self.open_chat(id, window, cx),
            Pick::StartAgent(project_id, agent) => {
                let path = self
                    .sidebar
                    .read(cx)
                    .projects()
                    .iter()
                    .find(|project| project.id == project_id)
                    .map(|project| project.path.clone());
                // The search has no checkbox, so a chat started from it gets
                // a worktree as the settings say.
                if let Some(path) = path {
                    let worktree = cx.global::<Settings>().worktree_by_default
                        && worktree::repo(&path).is_some();
                    self.new_chat(
                        project_id,
                        agent,
                        &path,
                        worktree,
                        String::new(),
                        window,
                        cx,
                    );
                }
            }
            Pick::SelectProject(id) => {
                match self.db.restore_project(id) {
                    Ok(true) => self.refresh(cx),
                    Ok(false) => {}
                    Err(err) => eprintln!("can't restore project {id}: {err}"),
                }
                self.sidebar
                    .update(cx, |sidebar, cx| sidebar.select(id, cx))
            }
            Pick::Filter(filter) => self
                .sidebar
                .update(cx, |sidebar, cx| sidebar.set_filter(filter, cx)),
            // Sent from where the keys would have sent it: closing the search
            // gave the focus back to where it was, as the shell, unless it was
            // outside the workspace.
            Pick::Run(action) => {
                if !self.focus_handle.contains_focused(window, cx) {
                    self.focus_chat(window, cx);
                }
                window.dispatch_action(action, cx);
            }
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
        let saved = Settings::set_rail_width(&self.db, settings.rail_width, cx)
            .and_then(|()| Settings::set_project_names(&self.db, settings.project_names, cx));
        if let Err(err) = saved {
            eprintln!("can't save the rail's width: {err}");
        }
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
        if let Err(err) = Settings::set_shell_share(&self.db, share, cx) {
            eprintln!("can't save the shell's height: {err}");
        }
    }

    fn save_sidebar_width(&mut self, cx: &mut Context<Self>) {
        let width = f32::from(self.sidebar_width);
        if cx.global::<Settings>().sidebar_width == width {
            return;
        }
        if let Err(err) = Settings::set_sidebar_width(&self.db, width, cx) {
            eprintln!("can't save the sidebar's width: {err}");
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if let Some(reason) = self.broken.clone() {
            return render_broken(reason, cx);
        }
        let active = self
            .active
            .and_then(|id| self.chats.iter().find(|chat| chat.id == id));
        let window_width = window.viewport_size().width;
        let names = sidebar::names_width(window_width, cx);

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
                    .p_2()
                    .gap_2()
                    .child(self.render_toolbar(
                        window_width
                            - clamp_sidebar_width(self.sidebar_width, window_width, names)
                            - names,
                        window,
                        cx,
                    ))
                    .child(
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
                                match active {
                                    _ if self.settings_shown => {
                                        this.child(render_settings(window, cx))
                                    }
                                    Some(chat) => this.child(self.render_chat(chat, window, cx)),
                                    None => this.child(
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
                            .child(self.render_footer(window, cx)),
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

        // A window too short for even the least heights clips the chat
        // rather than drawing it under the footer.
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.chat_focus)
            .child(self.render_chat_header(chat, cx))
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
                            div()
                                .flex_1()
                                .min_h(MIN_TERMINAL_HEIGHT)
                                .pl_5()
                                .pr_2()
                                .pt_3()
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
        let (_, shell) = self.shown_shell(cx)?;
        let viewport = window.viewport_size().height;
        let workspace = cx.weak_entity();
        Some(
            v_flex()
                .relative()
                .flex_shrink(1.)
                .h(shell_height(cx.global::<Settings>().shell_share, viewport))
                .min_h(MIN_SHELL_HEIGHT)
                .child(
                    h_flex()
                        .flex_none()
                        .h_8()
                        .justify_end()
                        .pr_2()
                        .bg(theme.muted)
                        .border_b_1()
                        .border_color(theme.border)
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
                        .flex_1()
                        .min_h_0()
                        .pl_5()
                        .pr_2()
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
                ),
        )
    }

    /// The chat's title and folder, what its agent is doing, and what can be
    /// done with it.
    fn render_chat_header(&self, chat: &Chat, cx: &Context<Self>) -> impl IntoElement {
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
                            .font_family(appearance::SERIF)
                            .text_xl()
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(format!(
                                "{} · {}",
                                chat.agent.label(),
                                home_relative(&chat.cwd)
                            )),
                    ),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .flex_wrap()
                    .justify_end()
                    .gap_x_4()
                    .gap_y_2()
                    .when(chat.running(), |this| this.child(status_pill(tone, cx)))
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
            .max_w(px(480.))
            .h_8()
            .bg(theme.background)
            .icon(IconName::Search)
            .accessibility_label("Search chats, projects and commands")
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
                            .child("Search chats, projects and commands"),
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

        h_flex()
            .flex_none()
            .w_full()
            .gap_4()
            .justify_between()
            .child(search)
            .child(
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
            )
    }

    /// The shortcuts used most, and what the agents are doing.
    fn render_footer(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let summary = self.sidebar.read(cx).summary();
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
            // What the agents do comes first, so a narrow window cuts the
            // shortcuts instead.
            .child(div().flex_none().child(summary))
            .child(div().flex_1())
            .children(bound(&NextNeedingYou, "next that needs you"))
            .children(bound(&ShowRunning, "running"))
            .children(bound(&OpenSearch, "search"))
            .children(bound(&NewChat, "new chat"))
            .children(bound(&ToggleShell, "shell"))
            .child(key_hint("Ctrl+1…9".into(), "open project", cx))
            .child(key_hint("Alt+1…9".into(), "open chat", cx))
            .children(bound(&ToggleSettings, "settings"))
    }

    /// What shows while no chat is open: how to start one, and how to read
    /// the chats' dots.
    fn render_empty(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self
            .sidebar
            .read(cx)
            .selected()
            .map(|project| project.name.clone());
        let legend = [
            Tone::Working,
            Tone::NeedsYou(None),
            Tone::Done,
            Tone::Error,
            Tone::Idle,
            Tone::Saved,
        ];

        h_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                v_flex()
                    .max_w(px(480.))
                    .gap_6()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().font_family(appearance::SERIF).text_3xl().child(
                                match selected {
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
                                            "Start Claude Code, Codex or Pi in {name}, or pick a \
                                             chat on the left to reopen it where it left off. \
                                             Agents keep running while you look at another chat."
                                        ),
                                        None => "A project is a folder you work in. Add one, \
                                                 then start Claude Code, Codex or Pi there."
                                            .to_string(),
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(match selected {
                                Some(_) => Button::new("empty-new-chat")
                                    .primary()
                                    .icon(IconName::Plus)
                                    .label("New chat")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.new_chat_here(&NewChat, window, cx)
                                    })),
                                None => Button::new("empty-add-project")
                                    .primary()
                                    .icon(IconName::FolderOpen)
                                    .label("Add project")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_project(window, cx)
                                    })),
                            })
                            .child(
                                Button::new("empty-search")
                                    .outline()
                                    .icon(IconName::Search)
                                    .label(match palette::shortcut(&OpenSearch, window) {
                                        Some(keys) => format!("Search  {keys}"),
                                        None => "Search".into(),
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_palette(false, window, cx)
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x_5()
                            .gap_y_2()
                            .pt_5()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .children(legend.map(|tone| {
                                h_flex()
                                    .gap_2()
                                    .child(sidebar::dot(tone, cx))
                                    .child(capitalized(match tone {
                                        Tone::NeedsYou(_) => "needs you",
                                        Tone::Error => "error",
                                        tone => tone.label(),
                                    }))
                            })),
                    ),
            )
    }
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

/// What the agent is doing, on its status color.
fn status_pill(tone: Tone, cx: &App) -> impl IntoElement {
    let palette = appearance::palette(cx);
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
        .child(sidebar::dot(tone, cx))
        .child(capitalized(tone.label()))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Records the sessions a chat's CLI reported last, and reads `sessions`
/// along with the ones those reports moved the chat into or out of. One
/// transcript that can't be read doesn't stop the others.
fn import_chat(
    db: &Db,
    reports: &mut Reports,
    id: i64,
    agent: Agent,
    key: &str,
    mut sessions: Vec<i64>,
) -> anyhow::Result<()> {
    let mut failure = None;
    for (agent_id, report) in reports.read(key, agent) {
        let path = &report.transcript_path;
        match db.record_session(id, agent, &agent_id, path, report.reason.as_deref()) {
            Ok(recorded) => sessions.extend(recorded.left.into_iter().chain([recorded.session_id])),
            Err(err) => {
                failure.get_or_insert(anyhow::Error::from(err));
            }
        }
    }
    sessions.sort_unstable();
    sessions.dedup();
    for session in sessions {
        if let Err(err) = transcript::read(db, session) {
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
fn render_import(projects: &[db::Project], theme: &Theme) -> impl IntoElement {
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

fn setting_not_saved(err: rusqlite::Error, window: &mut Window, cx: &mut App) {
    eprintln!("can't save the settings: {err}");
    window.push_notification(
        format!("Couldn't save the setting, so it lasts until Agents Hub quits: {err}"),
        cx,
    );
}

fn render_settings(window: &Window, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let Settings {
        close_to_tray,
        appearance: current,
        theme: family,
        notification_popups,
        notification_sound,
        worktree_by_default,
        ..
    } = *cx.global::<Settings>();
    let hint = |text: &'static str| {
        div()
            .pl_6()
            .text_sm()
            .line_height(rems(1.4))
            .text_color(theme.muted_foreground)
            .child(text)
    };
    let section = |title: &'static str| {
        v_flex()
            .max_w(px(520.))
            .gap_3()
            .child(div().font_semibold().child(title))
    };
    let mono = theme.mono_font_family.clone();
    let shortcut_row = |label: &'static str, keys: SharedString| {
        h_flex()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(div().flex_1().child(label))
            .child(
                div()
                    .px_1p5()
                    .rounded(px(4.))
                    .bg(appearance::palette(cx).strong)
                    .text_xs()
                    .font_family(mono.clone())
                    .child(keys),
            )
    };
    let bound: [(&'static str, Box<dyn Action>); 11] = [
        ("Search chats, projects and commands", Box::new(OpenSearch)),
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
    ];

    v_flex()
        .id("settings")
        .size_full()
        .overflow_y_scroll()
        .px_10()
        .py_8()
        .gap_8()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .font_family(appearance::SERIF)
                        .text_3xl()
                        .child("Settings"),
                )
                .child(
                    Button::new("close-settings")
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip_with_action("Close the settings", &ToggleSettings, None)
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.show_settings(false, window, cx)
                            }),
                        ),
                ),
        )
        .child(
            section("Theme").child(
                RadioGroup::horizontal("theme")
                    .children(ThemeFamily::ALL.map(ThemeFamily::label))
                    .selected_index(ThemeFamily::ALL.iter().position(|choice| *choice == family))
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        this.set_theme(ThemeFamily::ALL[*index], window, cx)
                    })),
            ),
        )
        .child(
            section("Appearance").child(
                RadioGroup::horizontal("appearance")
                    .children(Appearance::ALL.map(Appearance::label))
                    .selected_index(Appearance::ALL.iter().position(|choice| *choice == current))
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        this.set_appearance(Appearance::ALL[*index], window, cx)
                    })),
            ),
        )
        .child(
            section("Window")
                .child(
                    Checkbox::new("close-to-tray")
                        .label("Keep running in the tray when the window is closed")
                        .checked(close_to_tray)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            if let Err(err) = Settings::set_close_to_tray(&this.db, *checked, cx) {
                                setting_not_saved(err, window, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(hint(
                    "Agents keep working while the window is closed. Choose Agents Hub in \
                     the tray to open it again, or Quit there to stop them.",
                )),
        )
        .child(
            section("Chats")
                .child(
                    Checkbox::new("worktree-by-default")
                        .label("Start new chats in a new git worktree")
                        .checked(worktree_by_default)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            if let Err(err) =
                                Settings::set_worktree_by_default(&this.db, *checked, cx)
                            {
                                setting_not_saved(err, window, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(hint(
                    "In projects that use git, each new chat gets a branch and a checkout of \
                     its own, so agents don't edit each other's files. You can still untick \
                     it when you start a chat. Deleting the chat removes its worktree unless \
                     it has changes nobody committed; its branch stays.",
                )),
        )
        .child(
            section("Notifications")
                .child(
                    Checkbox::new("notification-popups")
                        .label("Show a notification")
                        .checked(notification_popups)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            if let Err(err) =
                                Settings::set_notification_popups(&this.db, *checked, cx)
                            {
                                setting_not_saved(err, window, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("notification-sound")
                        .label("Play a sound")
                        .checked(notification_sound)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            if let Err(err) =
                                Settings::set_notification_sound(&this.db, *checked, cx)
                            {
                                setting_not_saved(err, window, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(hint(
                    "When an agent finishes, needs you or stops on an error while you're \
                     looking elsewhere. Clicking the notification opens its chat.",
                ))
                .child(
                    h_flex().pl_6().pt_1().child(
                        Button::new("test-notification")
                            .small()
                            .label("Try it")
                            .disabled(!notification_popups && !notification_sound)
                            .on_click(|_, _, cx| alerts::test(cx)),
                    ),
                ),
        )
        .child(
            section("Keyboard shortcuts")
                .child(
                    div()
                        .text_sm()
                        .line_height(rems(1.4))
                        .text_color(theme.muted_foreground)
                        .child(
                            "They work while an agent has the keyboard, so they use keys \
                             terminal programs leave alone.",
                        ),
                )
                .child(
                    v_flex()
                        .children(bound.iter().filter_map(|(label, action)| {
                            palette::shortcut(action.as_ref(), window)
                                .map(|keys| shortcut_row(label, keys))
                        }))
                        .child(shortcut_row(
                            "Open the chat at that place in the list",
                            "Alt+1 … Alt+9".into(),
                        ))
                        .child(shortcut_row(
                            "Select the project at that place in the rail",
                            "Ctrl+1 … Ctrl+9".into(),
                        )),
                ),
        )
        .child(
            section("Data")
                .child(
                    div()
                        .text_sm()
                        .line_height(rems(1.4))
                        .text_color(theme.muted_foreground)
                        .child(
                            "Chats and their history live in one database. Export writes a \
                             copy of it where you choose. Import replaces it with one Agents \
                             Hub wrote, such as an export, and backs it up first.",
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("export-database")
                                .small()
                                .icon(IconName::HardDrive)
                                .label("Export a copy of the database")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.export_database(window, cx)
                                })),
                        )
                        .child(
                            Button::new("import-database")
                                .small()
                                .label("Import a database")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.import_database(window, cx)
                                })),
                        ),
                ),
        )
        .child(
            div()
                .pt_5()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(concat!("Agents Hub ", env!("CARGO_PKG_VERSION"))),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SHELL_SHARE, MIN_SHELL_HEIGHT, PendingRemoval, import_chat, shell_height, shell_share,
        take_and_remove,
    };
    use crate::worktree::{Removal, Worktree};
    use crate::{agent::Agent, db::Db, reports::Reports, transcript};
    use gpui_kit::px;
    use serde_json::json;
    use std::fs;
    use std::sync::{Arc, Mutex};

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

        import_chat(&db, &mut reports, chat, Agent::Claude, "cli", Vec::new()).unwrap();
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
        assert!(import_chat(&db, &mut reports, chat, Agent::Claude, "cli", sessions).is_err());
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
        let removal = PendingRemoval {
            job: Arc::new(Mutex::new(Some((worktree, dir.clone())))),
            waits: Vec::new(),
        };
        assert!(removal.is_pending());
        // Whichever of its task and shutdown comes first removes it.
        assert_eq!(
            take_and_remove(&removal.job).unwrap().unwrap(),
            Removal::Removed
        );
        assert!(take_and_remove(&removal.job).is_none());
        assert!(!removal.is_pending());
        fs::remove_dir_all(dir).ok();
    }
}
