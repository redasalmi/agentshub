//! Opening a chat.

use super::*;

impl Workspace {
    /// Shows a chat, starting its CLI unless it runs. The keyboard follows
    /// once the runtime shows it.
    pub fn open_chat(&mut self, id: i64, _: &mut Window, cx: &mut Context<Self>) {
        self.apply(Intent::OpenChat(id), cx);
    }
}
