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
use crate::bootstrap::Opened;
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
use crate::store::{self, Change, Store};
use crate::terminal::{self, Exited, Pty, StatusChanged, Terminal};
use crate::terminal_view::{self, TerminalView};
use crate::tray;
use crate::worktree::{self, Worktree};

mod actions;
mod catalog;
mod chat_view;
mod chats;

use catalog::Catalog;
mod codex;
mod commands;
mod dialogs;
mod feedback;
mod format;
mod layout;
mod new_chat;
mod open_chat;
mod palette_model;
mod preferences;
mod projects;
mod render;
mod search;
mod session;
mod settings_view;
mod shell;
mod shutdown;
mod startup;
mod status;
mod terminal_registry;
mod terminals;

pub use actions::*;
pub(crate) use format::{Turn, capitalized, duration, plural};
pub(crate) use session::import_chat;
#[cfg(test)]
pub(crate) use session::reported_sessions;

use crate::lifecycle::{Added, Codex, Deleted, Lifecycle, Reaped, Saved, Saving};

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
    /// The projects and chats, including the changes not saved yet.
    catalog: Catalog,
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
    crate::lifecycle::take_and_remove(job, worktree::remove)
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

/// A chat's backend state: what its CLI is doing and which session it is in.
struct ChatRuntime {
    id: i64,
    agent: Agent,
    /// Its project's folder, which its CLI runs in.
    cwd: PathBuf,
    /// The name of the file its CLI reports its sessions to.
    report: String,
    /// Startup failure or exit message, shown alongside the saved history.
    message: Option<SharedString>,
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

/// A chat's view state: its terminal and the history above it.
struct ChatPane {
    terminal: Option<ChatTerminal>,
    history: Entity<History>,
}

struct Chat {
    runtime: ChatRuntime,
    pane: ChatPane,
}

impl std::ops::Deref for Chat {
    type Target = ChatRuntime;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl std::ops::DerefMut for Chat {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.runtime
    }
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
        self.pane
            .terminal
            .as_ref()
            .is_some_and(|terminal| !terminal.exited)
    }

    fn current_session(&self) -> Option<i64> {
        self.session.filter(|_| self.running())
    }
}

const SHORTCUTS_WORK_ANYWHERE: &str = "They work while an agent has the keyboard, so they use \
                                        keys terminal programs leave alone.";

#[cfg(test)]
mod tests;
