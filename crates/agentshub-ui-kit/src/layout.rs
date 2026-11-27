//! What the workspace asks of the layout around it, and what the layout asks
//! of the workspace.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::menu::PopupMenu;
use gpui_kit::*;

use agentshub_core::status::Status;
use agentshub_core::store::{Chat, Project};
use agentshub_runtime::Runtime;

use crate::chat_body::ChatBody;
use crate::palette::{Entry, Pick};
use crate::settings::SettingsView;
use crate::status::Tone;

/// How a layout lists the projects and the chats around the parts every
/// layout shows, which the workspace hands it.
pub trait LayoutView {
    /// The project new chats, the shell and the empty state are for.
    fn selected_project(&self, cx: &App) -> Option<i64>;
    /// Makes project `project` the selected one, telling the workspace with
    /// `NavEvent::Selected` if it wasn't.
    fn select_project(&self, project: i64, cx: &mut App);
    /// Selects the `index`th project, counting from zero, as Ctrl and a
    /// number do.
    fn select_project_at(&self, index: usize, cx: &mut App);
    /// The `index`th chat of the list, counting from zero, which Alt and a
    /// number open.
    fn chat_at(&self, index: usize, cx: &App) -> Option<i64>;
    /// The chat to go to next among the ones that need the user: the one
    /// after chat `after` in the layout's order, or the first.
    fn next_needing_you(&self, after: Option<i64>, cx: &App) -> Option<i64>;
    /// Reads the projects, the chats and what their agents do from the
    /// runtime again.
    fn refresh(&self, cx: &mut App);
    /// Brings chat `chat`, which is being shown, into view.
    fn reveal(&self, chat: i64, cx: &mut App);
    /// Shows the numbers the held modifier opens chats or projects with, or
    /// hides them.
    fn hold_numbers(&self, held: Option<Held>, cx: &mut App);
    /// The search box's commands that are the layout's own.
    fn commands(&self, cx: &App) -> Commands;
    /// Does what `pick`, one of `commands`' picks the workspace doesn't
    /// run itself, says.
    fn pick(&self, pick: Pick, cx: &mut App);
    /// Runs `action`, which a search box command dispatches, if it is one
    /// of the layout's own, and says whether it was. The workspace draws
    /// itself again after one that was.
    fn handle_action(&self, action: &dyn Action, window: &mut Window, cx: &mut App) -> bool;
    /// The layout, around `parts`.
    fn render(&self, parts: Parts, window: &mut Window, cx: &mut App) -> AnyElement;
}

/// The search box's commands that are a layout's own, by where they show
/// among the workspace's.
#[derive(Default)]
pub struct Commands {
    /// Among the ones that find chats, before the running chats'.
    pub find: Vec<Entry>,
    /// Among the ones that change how the app looks, before the theme's.
    pub look: Vec<Entry>,
}

/// Which shortcut numbers the layout shows, while the modifier that opens
/// them is held: Alt for the chats, Ctrl for the projects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    Chats,
    Projects,
}

/// What a layout asks the workspace to do.
#[derive(Clone)]
pub enum NavEvent {
    /// Ask for a folder to add as a project.
    AddProject,
    /// Files and folders dropped on the layout from outside the app; the
    /// folders among them become projects.
    DropPaths(Vec<PathBuf>),
    /// Remove a project, once the user agrees.
    RemoveProject(i64),
    /// Archive a project, with its chats.
    ArchiveProject(i64),
    /// Move a project to where another one is.
    MoveProject(i64, i64),
    /// Start a chat in a project.
    NewChat(i64),
    /// Show a chat, starting its CLI unless it runs.
    OpenChat(i64),
    /// Ask for a chat's new title, starting from its current one.
    RenameChat(i64, SharedString),
    /// Pin a chat to the top of its list, or unpin it.
    PinChat(i64, bool),
    /// Another project was selected, or none.
    Selected,
    /// Move a chat to where another one is.
    MoveChat(i64, i64),
    /// Stop a running chat's agent, which opening the chat again resumes.
    CloseChat(i64),
    /// Delete a chat, once the user agrees.
    DeleteChat(i64),
    /// Archive a chat, which leaves the list, stopping its agent.
    ArchiveChat(i64),
    /// Take a chat out of the archive, to the top of its list.
    RestoreChat(i64),
    /// Show the settings in place of the chat.
    OpenSettings,
    /// Open the search box.
    OpenSearch,
    /// Open the running chats' menu, or close it.
    RunningMenu(bool),
    /// Close the window, remembering its bounds for when it opens again.
    CloseWindow,
}

/// What the workspace hands its layout to lay out.
pub struct Parts {
    /// What the chat's sheet shows.
    pub body: Body,
    /// The running chats' menu, while it is open.
    pub running_menu: Option<Entity<PopupMenu>>,
}

/// What the chat's sheet shows.
pub enum Body {
    /// The settings, which the layout draws as wide as the room it leaves
    /// them.
    Settings(Entity<SettingsView>),
    /// The shown chat, which the layout draws its header over.
    Chat(Box<ShownChat>),
    /// What shows while no chat does: the empty state, over the shell.
    Empty(AnyElement),
}

/// What the workspace holds of the shown chat. The layout reads the rest of
/// what its header shows from the runtime.
pub struct ShownChat {
    /// Tracks the chat, its header and its body alike, so the workspace can
    /// tell whether the keyboard is in it.
    pub focus: FocusHandle,
    /// How many earlier sessions the chat has.
    pub earlier: usize,
    /// Whether they show.
    pub history_open: bool,
    /// What shows under the header.
    pub body: ChatBody,
}

/// What each chat whose CLI runs is doing, as the layouts show it: a CLI
/// that finished shows as done until its chat is seen, and as idle after.
pub fn running(runtime: &Runtime) -> HashMap<i64, Option<Status>> {
    runtime
        .open_chats()
        .filter(|chat| chat.running)
        .map(|chat| {
            let status = match chat.status {
                Some(Status::Idle | Status::Done) if chat.unseen => Some(Status::Done),
                Some(Status::Done) => Some(Status::Idle),
                status => status,
            };
            (chat.id, status)
        })
        .collect()
}

/// The running chats of the projects not archived, the most urgent first,
/// and in the projects' order within the same urgency, so a chat only moves
/// when what its agent does changes. `running` is what each one's agent is
/// doing, as `running` reads it.
pub fn running_chats<'a>(
    projects: &'a [Project],
    running: &HashMap<i64, Option<Status>>,
) -> Vec<(&'a Project, &'a Chat, Tone)> {
    let mut chats: Vec<_> = projects
        .iter()
        .filter(|project| project.archived_at.is_none())
        .flat_map(|project| {
            project
                .chats
                .iter()
                .filter(|chat| chat.archived_at.is_none())
                .filter_map(move |chat| {
                    let running = running.get(&chat.id).copied()?;
                    Some((project, chat, Tone::of(Some(running))))
                })
        })
        .collect();
    chats.sort_by_key(|(_, _, tone)| tone.urgency());
    chats
}
