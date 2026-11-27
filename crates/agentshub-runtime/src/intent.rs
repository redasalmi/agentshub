//! What the window asks the runtime to do.

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
}
