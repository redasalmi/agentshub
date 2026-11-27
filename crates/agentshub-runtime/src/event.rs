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
    /// The CLI of the terminal with this report name reported the sessions
    /// it went into, which `Runtime::read_reports` takes out.
    #[doc(hidden)]
    Reported(String),
    /// Codex wrote its session files, so the sessions of its CLIs are looked
    /// for with `Runtime::find_codex_sessions`.
    #[doc(hidden)]
    CodexChanged,
}
