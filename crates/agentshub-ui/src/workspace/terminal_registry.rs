//! Every terminal the workspace owns: the chats' and the projects' shells.

use crate::terminal::Colors;

use super::*;

/// A view of the workspace's terminal entities.
pub(super) struct TerminalRegistry<'a> {
    chats: &'a [Chat],
    shells: &'a HashMap<PathBuf, Shell>,
}

impl Workspace {
    pub(super) fn terminals(&self) -> TerminalRegistry<'_> {
        TerminalRegistry {
            chats: &self.chats,
            shells: &self.shells,
        }
    }
}

impl TerminalRegistry<'_> {
    /// The terminal of every chat and shell.
    pub(super) fn all(&self) -> impl Iterator<Item = &Entity<Terminal>> {
        self.chats
            .iter()
            .filter_map(|chat| chat.pane.terminal.as_ref())
            .map(|terminal| &terminal.terminal)
            .chain(self.shells.values().map(|shell| &shell.terminal))
    }

    /// Paints every terminal in `colors`.
    pub(super) fn set_colors(&self, cx: &App, colors: Colors) {
        for terminal in self.all() {
            terminal.read(cx).set_colors(colors);
        }
    }
}
