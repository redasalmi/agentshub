//! What the window asks the runtime to do.

use agentshub_core::store::Change;

/// What a view asks the runtime to do.
pub enum Intent {
    /// Shows a change to the projects and chats at once, and saves it.
    Change(Change),
}
