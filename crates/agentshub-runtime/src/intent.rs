//! What the window asks the runtime to do.

use std::path::PathBuf;

use agentshub_app::settings::Value;
use agentshub_core::agent::Agent;
use agentshub_core::editors::Editor;
use agentshub_core::store::{Change, PendingImport};
use agentshub_core::worktree::Worktree;

use crate::IdleCheck;

/// What a view asks the runtime to do.
#[derive(Clone)]
pub enum Intent {
    /// Shows a change to the projects and chats at once, and saves it.
    Change(Change),
    /// Shows a chat, starting its CLI unless it runs.
    OpenChat(i64),
    /// Starts a chat's CLI again.
    RestartChat(i64),
    /// Closes a chat whose CLI the user quit.
    CloseChat(i64),
    /// Kills a chat's CLI and closes the chat.
    StopChat(i64),
    /// Closes a chat after recording the session its CLI is in, which
    /// opening it again resumes.
    Suspend(i64),
    /// Whether the user can see the shown chat: the window has the focus,
    /// and the settings don't hide the chat. While the window is closed to
    /// the tray (see `SetHidden`), the user can't.
    SetFocused(bool),
    /// The window closed, which drops what it asked to open and was still
    /// waiting for: a chat whose last session is being read, or whose stopped
    /// CLI is being reaped, doesn't start once that's done.
    WindowClosed,
    /// Starts an agent in a new chat of a project, as the New Chat dialog
    /// asks: titled `title`, or after its agent's sessions without one, and
    /// in a new worktree of its own if `worktree` is set.
    NewChat {
        project: i64,
        agent: Agent,
        title: Option<String>,
        worktree: bool,
    },
    /// Starts an agent in a new chat of a project without asking anything
    /// first, in a new worktree of its own if the settings say so and the
    /// project is in a git repository.
    StartAgent { project: i64, agent: Agent },
    /// Looks up the agents a project can start (see `Runtime::installed`).
    LookUpInstalled(i64),
    /// Stops a project's agents, and removes it with its chats and the
    /// worktrees Agents Hub made for them.
    RemoveProject(i64),
    /// Closes a project's chats, and archives it with them.
    ArchiveProject(i64),
    /// Closes a chat, and archives it.
    ArchiveChat(i64),
    /// Stops a chat's agent, and deletes the chat with `worktree`, the one
    /// Agents Hub made for it in its project's folder.
    DeleteChat {
        chat: i64,
        worktree: Option<(Worktree, PathBuf)>,
    },
    /// Closes chats, as `Suspend` closes each.
    CloseChats(Vec<i64>),
    /// Closes the chats whose CLI sits at rest with nothing the user would
    /// miss, the shown one included, by what `Runtime::check_idle` read.
    CloseIdle(IdleCheck),
    /// Closes every open chat, stopping its CLI even mid-turn, and the ones
    /// still opening or waiting for their worktree.
    CloseAll,
    /// Adds folders as projects, in the order given at the top of the list,
    /// and selects the first.
    AddFolders(Vec<PathBuf>),
    /// Copies the database to a file.
    Export(PathBuf),
    /// Checks a database to import, and copies it (see
    /// `RuntimeEvent::ImportChecked`).
    Import(PathBuf),
    /// Puts a checked database in place of this one, and has the app
    /// restart to use it.
    ConfirmImport(PendingImport),
    /// Saves settings the window applied, as each setter of `Settings` gives
    /// them: if one can't be saved, it lasts until the app quits.
    SaveSettings(Vec<(&'static str, Value)>),
    /// The sidebar selected another project, or none, whose shell the panel
    /// is for (see `Runtime::shell`). Selecting one is asking to show it, so
    /// a chat asked for before doesn't take the view once it opens.
    ProjectSelected(Option<i64>),
    /// Whether the settings cover the terminals, so that none of them builds
    /// frames. The tray covers them too (see `SetHidden`).
    SetCovered(bool),
    /// The window was closed to the tray, or opened from it. While it is
    /// closed, no terminal builds frames, and the user can't see the shown
    /// chat.
    SetHidden(bool),
    /// The settings show in place of the chat, or the chat again in their
    /// place. A chat still opening doesn't take the view from the settings.
    ShowSettings(bool),
    /// Lists the projects and chats again as the database has them (see
    /// `RuntimeEvent::Listed`), unless a change shown isn't saved yet:
    /// saving it lists them.
    ListProjects,
    /// Shows the shell the panel is for, starting it if it isn't running, or
    /// hides it if it shows and nothing covers it (see `Runtime::shell`).
    ToggleShell,
    /// Opens the folder a chat works in, the shown one without `chat`, in a
    /// code editor, the default one without `editor`.
    OpenInEditor {
        chat: Option<i64>,
        editor: Option<Editor>,
    },
}
