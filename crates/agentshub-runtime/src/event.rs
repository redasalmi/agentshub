//! What the runtime tells the window about.

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
}
