//! Managing the projects and chats: adding, removing, archiving and closing
//! them, the database file behind them, and the settings.

use agentshub_core::status::Status;

use crate::ChatState;

/// The chats among `states` whose CLI runs at rest with nothing unseen,
/// which closing the idle chats closes if stopping them loses nothing else.
pub(crate) fn pick_idle(states: &[ChatState]) -> Vec<i64> {
    states
        .iter()
        .filter(|chat| {
            chat.running && !chat.unseen && matches!(chat.status, Some(Status::Idle | Status::Done))
        })
        .map(|chat| chat.id)
        .collect()
}
