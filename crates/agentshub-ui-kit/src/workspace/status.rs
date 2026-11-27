//! Keeping the chats' histories in step with what each agent does, and
//! telling the runtime what the user can see.

use super::*;

impl Workspace {
    /// Lists the projects and chats again, and the earlier sessions of the
    /// chats whose CLI runs.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_after(&[], cx);
    }

    /// The same, reloading the expanded sessions whose transcript read, in
    /// `changed`, found something.
    pub(super) fn refresh_after(&mut self, changed: &[i64], cx: &mut Context<Self>) {
        self.apply(Intent::ListProjects, cx);
        let runtime = self.runtime.read(cx);
        let histories: Vec<_> = self
            .panes
            .iter()
            .map(|(id, pane)| (pane.history.clone(), runtime.current_session(*id)))
            .collect();
        for (history, current) in histories {
            history.update(cx, |history, cx| history.refresh(current, changed, cx));
        }
    }

    /// Tells the runtime whether the user can see the shown chat: `focused`
    /// says whether the window has the focus, and the settings may cover the
    /// chat. `focused` is passed in because, on X11, the window's activation
    /// is reported before `cx.active_window()` catches up with it.
    pub fn set_focused(&self, focused: bool, cx: &mut App) {
        let seen = focused && !self.settings_shown;
        self.apply(Intent::SetFocused(seen), cx);
    }
}
