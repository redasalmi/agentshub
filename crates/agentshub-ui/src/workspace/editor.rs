//! Opening a chat's folder in a code editor.

use super::*;

impl Workspace {
    /// Opens the shown chat's folder in the default editor.
    pub(super) fn open_in_editor(
        &mut self,
        _: &OpenInEditor,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply(
            Intent::OpenInEditor {
                chat: None,
                editor: None,
            },
            cx,
        );
    }
}
