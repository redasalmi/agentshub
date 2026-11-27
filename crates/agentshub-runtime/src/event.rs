//! What the runtime tells the window about.

use std::path::PathBuf;

use agentshub_core::lifecycle::Reaped;
use agentshub_core::worktree::Worktree;
use gpui_kit::SharedString;

use crate::Intent;

/// Something the runtime changed, or has to tell the user, for the window to
/// show.
pub enum RuntimeEvent {
    /// The projects and chats changed, or a change to them was saved.
    CatalogChanged,
    /// Transcripts were read, so the projects list again. `sessions` are the
    /// ones whose read found something, which the chats' histories reload
    /// where they show them.
    HistoryChanged { sessions: Vec<i64> },
    /// Something to tell the user in a notification.
    Notice(String),
    /// Something to tell the user in a notification with a button that
    /// undoes it, by asking for `undo`.
    Undoable { message: String, undo: Intent },
    /// Chat `id` opened, with its terminal if its CLI started, so its pane
    /// can be built.
    ChatStarted(i64),
    /// Chat `id`'s CLI exited, and its terminal stays with what it wrote.
    ChatExited(i64),
    /// Chat `id` closed, and its terminal with it.
    ChatEnded(i64),
    /// What chat `id`'s CLI does changed.
    StatusChanged(i64),
    /// The chat now shown, or that none is. Showing the chat already shown
    /// says so too, since it is asked for again.
    ActiveChanged(Option<i64>),
    /// The shown chat's turn went on another second.
    TurnTick,
    /// Chats whose Codex CLI exited while a lookup of its session ran, which
    /// has now recorded it, so they close: the window closes them as it closes
    /// the ones the user does, with `Intent::Suspend`, and gives back the
    /// keyboard they had.
    ClosingAfterLookup(Vec<i64>),
    /// The agents project `id` can start were looked up (see
    /// `Runtime::installed`).
    InstalledChanged(i64),
    /// `project` is being copied into a new worktree for a new chat, which
    /// takes seconds for a large one, with nothing else to show that the chat
    /// is coming. The window says so until `CopiedProject` with the same
    /// `id`.
    CopyingProject { id: SharedString, project: PathBuf },
    /// The copy `CopyingProject` told about with this id is done.
    CopiedProject(SharedString),
    /// A new chat is shown in project `id`, which the sidebar selects: it
    /// doesn't list the chat yet, so it can't find its project by itself.
    SelectProject(i64),
    /// A deleted chat's worktree, in `project`'s folder, is to be removed.
    /// The window stops the shells running in it, and has the runtime remove
    /// it once they and the chat's CLI, if it had one (`reaped`), have been
    /// reaped (see `Runtime::remove_worktree`).
    RemovingWorktree {
        worktree: Worktree,
        project: PathBuf,
        reaped: Option<Reaped>,
    },
    /// The project in this folder is being removed, so the window stops its
    /// shell.
    RemovingProject(PathBuf),
}
