//! Notifications, undo offers and where the keyboard goes.

use super::*;

impl Workspace {
    /// Says so if a setting couldn't be saved, which then lasts until Agents
    /// Hub quits.
    pub(super) fn save_setting(
        &self,
        saved: impl Future<Output = anyhow::Result<()>> + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Err(err) = saved.await {
                eprintln!("can't save the settings: {err:#}");
                let message =
                    format!("Couldn't save the setting, so it lasts until Agents Hub quits: {err}");
                this.update(cx, |this, cx| this.say(message, cx)).ok();
            }
        })
        .detach();
    }

    /// Says `message` in the window, unless it is closed to the tray.
    pub(super) fn say(&self, message: String, cx: &mut App) {
        self.window
            .update(cx, |_, window, cx| window.push_notification(message, cx))
            .ok();
    }

    /// Says `message` with a button that makes the `undo` change.
    pub(super) fn offer_undo(
        &self,
        message: &'static str,
        undo: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        window.push_notification(
            Notification::new()
                .message(message)
                .action(move |_, _, cx| {
                    let (this, undo) = (this.clone(), undo.clone());
                    Button::new("undo").label("Undo").on_click(cx.listener(
                        move |note, _, window, cx| {
                            this.update(cx, |this, cx| this.change(undo.clone(), cx))
                                .ok();
                            note.dismiss(window, cx);
                        },
                    ))
                })
                // A button keeps it up unless it's told otherwise.
                .autohide(true),
            cx,
        );
    }

    /// Focuses the shown chat's CLI, or the workspace if it has none running
    /// or the settings cover it. A terminal out of the rendered tree can't
    /// hold the focus: keys would skip the workspace's bindings.
    pub fn focus_chat(&self, window: &mut Window, cx: &mut App) {
        match self
            .chats
            .iter()
            .find(|chat| Some(chat.id) == self.active)
            .and_then(|chat| chat.pane.terminal.as_ref())
            .filter(|_| !self.settings_shown)
        {
            Some(terminal) => terminal.view.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }
}
