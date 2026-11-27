use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{HandleEdge, resize_handle};
use gpui_kit::component::{
    ActiveTheme as _, IconName, StyledExt as _, Theme, ThemeStyled as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::appearance;
use crate::history::History;
use crate::palette::{self, Entry, Lead, Pick};
use crate::settings::{SettingsIntent, SettingsView};
use crate::sidebar::{
    self, Held, MAX_SIDEBAR_WIDTH, MIN_MAIN_WIDTH, MIN_SIDEBAR_WIDTH, Sidebar, SidebarEvent, Tone,
    home_relative, rail_room,
};
use crate::terminal_view::{self, TerminalView};
use agentshub_app::alerts;
use agentshub_app::settings::{
    Appearance, DEFAULT_RAIL_WIDTH, DEFAULT_SIDEBAR_WIDTH, DEFAULT_TERMINAL_FONT_SIZE, Motion,
    Settings, ThemeFamily, Value,
};
use agentshub_core::agent::Agent;
use agentshub_core::status::Status;
use agentshub_core::store::{self, Change};
use agentshub_core::worktree::Worktree;
#[cfg(target_os = "linux")]
use agentshub_platform::scope;
use agentshub_runtime::terminal::{self, Exited, Pty, Terminal};
use agentshub_runtime::{Intent, Runtime, RuntimeEvent, take_and_remove};

mod actions;
mod catalog;
mod chat_view;
mod chats;
mod commands;
mod dialogs;
mod editor;
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
mod shell;
mod shutdown;
mod startup;
mod status;
mod terminals;

pub use actions::*;
pub(crate) use format::plural;

/// How many of the chats last worked in show while no chat is open.
const RECENT_CHATS: usize = 5;

/// How long Alt or Ctrl is held alone before the sidebar shows the numbers it
/// opens chats or projects with, so quick shortcuts don't flash them.
const REVEAL_NUMBERS_AFTER: Duration = Duration::from_millis(400);

/// Binds the shortcuts that reach the workspace.
pub fn init(cx: &mut App) {
    actions::init(cx);
}

pub struct Workspace {
    sidebar: Entity<Sidebar>,
    settings_view: Entity<SettingsView>,
    /// The store, the projects and chats, the open chats' CLIs, and the chats
    /// on their way in or out, which outlive the window.
    runtime: Entity<Runtime>,
    _runtime_events: Subscription,
    /// Tells the runtime when the window closes.
    _window_closed: Subscription,
    /// The panes of the runtime's open chats, by chat.
    panes: HashMap<i64, ChatPane>,
    _sidebar_events: Subscription,
    /// Shows the sidebar's shortcut numbers once their modifier has been
    /// held a moment on its own.
    reveal_numbers: Option<Task<()>>,
    /// The New Chat dialog's lookup under way, which opens the dialog once
    /// done.
    _new_chat_lookup: Option<Task<()>>,
    /// The search box last opened, with the project whose agents it offers
    /// to start, which fill in once they have been looked up.
    palette_start: Option<(i64, palette::Sections)>,
    _theme: Subscription,
    _focus_lost: Subscription,
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

/// A chat's view state: its terminal and the history above it.
struct ChatPane {
    /// The view of its terminal, unless its CLI didn't start.
    view: Option<Entity<TerminalView>>,
    history: Entity<History>,
}

#[cfg(test)]
mod tests;
