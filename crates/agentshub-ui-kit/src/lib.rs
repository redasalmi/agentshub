//! What the layouts of the app share: the actions and their shortcuts, the
//! theme, the terminal view, the history, the status glyphs, the chat body,
//! the empty state and the interface the binary opens a layout through.

pub mod actions;
pub mod appearance;
pub mod chat_body;
pub mod dialogs;
pub mod editor;
pub mod empty;
pub mod frontend;
pub mod history;
pub mod palette;
pub mod settings;
pub mod shortcuts;
pub mod status;
pub mod terminal_view;
pub mod theme;
pub mod widgets;

pub use frontend::Frontend;
