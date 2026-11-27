//! What runs behind the UI: the chats' CLIs and terminals, and the state they change.

pub mod terminal;

mod chats;
mod event;
mod intent;
mod new_chat;
mod runtime;
mod sessions;

pub use chats::{ChatState, IdleCheck, take_and_remove};
pub use event::RuntimeEvent;
pub use intent::Intent;
pub use new_chat::Asked;
pub use runtime::Runtime;

#[cfg(test)]
mod tests;
