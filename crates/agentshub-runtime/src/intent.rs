//! What the window asks the runtime to do.

use agentshub_core::agent::Agent;
use agentshub_core::store::Change;

/// What a view asks the runtime to do.
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
    /// and neither the settings nor the tray hide the chat.
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
}
