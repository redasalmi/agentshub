use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, IconName, StyledExt as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{
    AddProject, ArchiveChat, ArchiveProject, CloseAllChats, CloseIdleChats, DeleteChat,
    ExportDatabase, ImportDatabase, NewChat, NextNeedingYou, OpenChatAt, OpenInEditor, OpenSearch,
    RenameChat, SelectProjectAt, ShowArchived, ShowRunning, ShowShortcuts, ToggleHistory,
    TogglePin, ToggleProjectNames, ToggleSettings, ToggleShell, ToggleTheme, UseTheme,
};
use crate::appearance;
use crate::history::History;
use crate::layout::{Body, Held, LayoutView, NavEvent, Parts, ShownChat};
use crate::palette::{self, Entry, Lead, Pick};
use crate::settings::{SettingsIntent, SettingsView};
use crate::status::{Tone, dot, subtitle};
use crate::terminal_view::{self, TerminalView};
use crate::widgets::group_heading;
use agentshub_app::alerts;
use agentshub_app::settings::{
    Appearance, DEFAULT_TERMINAL_FONT_SIZE, Motion, Settings, ThemeFamily, Value,
};
use agentshub_core::agent::Agent;
use agentshub_core::format::home_relative;
use agentshub_core::status::Status;
use agentshub_core::store::{self, Change};
use agentshub_runtime::terminal::Terminal;
use agentshub_runtime::{Intent, Runtime, RuntimeEvent};

mod chat_view;
mod chats;
mod commands;
mod dialogs;
mod editor;
mod feedback;
mod layout;
mod new_chat;
mod open_chat;
mod palette_model;
mod preferences;
mod projects;
mod render;
mod search;
mod shell;
mod startup;
mod status;
mod terminals;

/// How many of the chats last worked in show while no chat is open.
const RECENT_CHATS: usize = 5;

/// How long Alt or Ctrl is held alone before the layout shows the numbers it
/// opens chats or projects with, so quick shortcuts don't flash them.
const REVEAL_NUMBERS_AFTER: Duration = Duration::from_millis(400);

/// Binds the shortcuts that reach the workspace.
pub fn init(cx: &mut App) {
    crate::actions::bind_keys(cx);
}

pub struct Workspace {
    /// Lists the projects and chats around the shown chat.
    layout: Box<dyn LayoutView>,
    settings_view: Entity<SettingsView>,
    /// The store, the projects and chats, the open chats' CLIs, and the chats
    /// on their way in or out, which outlive the window.
    runtime: Entity<Runtime>,
    _runtime_events: Subscription,
    /// Tells the runtime when the window closes.
    _window_closed: Subscription,
    /// The panes of the runtime's open chats, by chat.
    panes: HashMap<i64, ChatPane>,
    /// Shows the layout's shortcut numbers once their modifier has been
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
    /// The views of the runtime's shells, by folder.
    shell_views: HashMap<PathBuf, Entity<TerminalView>>,
    /// Whether the settings show in place of the chat.
    settings_shown: bool,
    /// The running chats' menu, while it is open.
    running_menu: Option<Entity<PopupMenu>>,
    /// Holds the focus when nothing in the workspace does, so its key
    /// bindings still reach it.
    focus_handle: FocusHandle,
    /// Tracks the shown chat, so closing it can tell whether the keyboard
    /// went with it, from its CLI or from its header's buttons alike.
    chat_focus: FocusHandle,
    /// Whether a database to import is being picked or checked.
    importing: bool,
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

/// A chat's view state: its terminal and the history above it.
struct ChatPane {
    /// The view of its terminal, unless its CLI didn't start.
    view: Option<Entity<TerminalView>>,
    history: Entity<History>,
}
