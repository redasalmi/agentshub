//! What runs behind the UI: the chats' CLIs and terminals, and the state they change.

pub mod terminal;

mod event;
mod intent;
mod runtime;
mod sessions;

pub use event::RuntimeEvent;
pub use intent::Intent;
pub use runtime::Runtime;
pub use sessions::{Recording, import_chat};

#[cfg(test)]
mod tests;
