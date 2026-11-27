//! What the runtime tells the window about.

/// Something the runtime changed, or has to tell the user, for the window to
/// show.
pub enum RuntimeEvent {
    /// The projects and chats changed, or a change to them was saved.
    CatalogChanged,
    /// Something to tell the user in a notification.
    Notice(String),
}
