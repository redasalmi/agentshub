//! Notifications, undo offers and where the keyboard goes.

use super::*;

impl Workspace {
    /// Has the runtime save a setting just applied, which then lasts until
    /// Agents Hub quits if it can't be.
    pub(super) fn save_setting(&self, saved: (&'static str, Value), cx: &mut App) {
        self.apply(Intent::SaveSettings(vec![saved]), cx);
    }

    /// Says `message` in the window, unless it is closed to the tray.
    pub(super) fn say(&self, message: String, cx: &mut Context<Self>) {
        cx.with_window(cx.entity_id(), |window, cx| {
            window.push_notification(message, cx)
        });
    }

    /// Says `message` with a button that asks the runtime for `undo`,
    /// unless the window is closed to the tray.
    pub(super) fn offer_undo(&self, message: String, undo: Intent, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let notification = Notification::new()
            .message(message)
            .action(move |_, _, cx| {
                let (this, undo) = (this.clone(), undo.clone());
                Button::new("undo").label("Undo").on_click(cx.listener(
                    move |note, _, window, cx| {
                        this.update(cx, |this, cx| this.apply(undo.clone(), cx))
                            .ok();
                        note.dismiss(window, cx);
                    },
                ))
            })
            // A button keeps it up unless it's told otherwise.
            .autohide(true);
        cx.with_window(cx.entity_id(), |window, cx| {
            window.push_notification(notification, cx)
        });
    }

    /// Focuses the shown chat's CLI, or the workspace if it has none running
    /// or the settings cover it. A terminal out of the rendered tree can't
    /// hold the focus: keys would skip the workspace's bindings.
    pub fn focus_chat(&self, window: &mut Window, cx: &mut App) {
        let view = self
            .runtime
            .read(cx)
            .active()
            .and_then(|id| self.panes.get(&id))
            .and_then(|pane| pane.view.clone())
            .filter(|_| !self.settings_shown);
        match view {
            Some(view) => view.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }
}
