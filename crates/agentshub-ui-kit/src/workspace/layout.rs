//! Resizing the shell.

use super::*;

impl Workspace {
    /// Shows the shell taking `share` of the window's height, without saving
    /// it.
    pub(super) fn resize_shell(&mut self, share: f32, cx: &mut Context<Self>) {
        let settings = *cx.global::<Settings>();
        if settings.shell_share == share {
            return;
        }
        cx.set_global(Settings {
            shell_share: share,
            ..settings
        });
        cx.notify();
    }

    pub(super) fn save_shell_share(&mut self, cx: &mut Context<Self>) {
        let share = cx.global::<Settings>().shell_share;
        let saved = Settings::set_shell_share(share, cx);
        self.save_setting(saved, cx);
    }
}
