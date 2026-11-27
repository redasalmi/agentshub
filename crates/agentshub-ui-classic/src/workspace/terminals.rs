//! The chats' panes, which follow the runtime's chats: their terminal views
//! and histories, and where the keyboard goes as chats show and close.

use super::*;

impl Workspace {
    /// Builds the pane of a chat that opened: its terminal's view, if its CLI
    /// started, and the history of the sessions it has.
    pub(super) fn chat_started(&mut self, id: i64, cx: &mut Context<Self>) {
        let runtime = self.runtime.read(cx);
        if runtime.chat(id).is_none() {
            return;
        }
        let terminal = runtime.terminal(id);
        // Without a running CLI, even its last session belongs in history.
        let current = runtime.current_session(id);
        let source = runtime.history_source();
        let blocked = terminal.is_none();
        let history = cx.new(|cx| {
            // Without a terminal, the history is all there is to show.
            let mut history = History::new(source, id, blocked);
            history.refresh(current, &[], cx);
            history
        });
        let view = terminal.map(|terminal| cx.new(|cx| TerminalView::new(terminal, cx)));
        self.panes.insert(id, ChatPane { view, history });
        self.layout.refresh(cx);
        cx.notify();
    }

    /// Takes in the last session of a chat whose CLI exited, whose terminal
    /// stays.
    pub(super) fn chat_exited(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(&id) {
            pane.history
                .update(cx, |history, cx| history.refresh(None, &[], cx));
        }
        self.layout.refresh(cx);
        cx.notify();
    }

    /// Drops the pane of a chat that closed. The keyboard goes with its
    /// terminal, so the workspace takes it back if the terminal had it.
    pub(super) fn chat_ended(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.remove(&id) else {
            return;
        };
        // Closed to the tray, the window has no keyboard to give back, and
        // opening it again focuses the shown chat.
        cx.with_window(cx.entity_id(), |window, cx| {
            if pane
                .view
                .as_ref()
                .is_some_and(|view| view.focus_handle(cx).contains_focused(window, cx))
            {
                self.focus_chat(window, cx);
            }
        });
        self.layout.refresh(cx);
        cx.notify();
    }

    /// Closes the chats whose Codex lookup finished after their CLI exited.
    /// One closed while its CLI was exiting may not be reaped yet, so its
    /// session is read once it is, and nothing else runs in that session
    /// until then. The shown chat takes the keyboard with it, which only its
    /// window can give back.
    pub(super) fn close_after_lookup(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) {
        let closed = cx.with_window(cx.entity_id(), |window, cx| {
            self.close_chats(ids.clone(), window, cx)
        });
        // Closed to the tray, the window has no keyboard to give back, and
        // opening it again focuses the shown chat.
        if closed.is_none() {
            for id in ids {
                self.apply(Intent::Suspend(id), cx);
            }
        }
    }

    /// Follows the chat the runtime shows, which comes in front of the
    /// settings and takes the keyboard.
    pub(super) fn active_changed(&mut self, active: Option<i64>, cx: &mut Context<Self>) {
        let Some(id) = active else {
            self.layout.refresh(cx);
            cx.notify();
            return;
        };
        self.settings_shown = false;
        let focused = cx
            .with_window(cx.entity_id(), |window, cx| {
                self.focus_chat(window, cx);
                window.is_window_active()
            })
            .unwrap_or(false);
        self.cover_terminals(cx);
        self.layout.refresh(cx);
        // Showing a chat, even the one already shown, brings its project back
        // into the rail's selection, so the chat list and New Chat follow it.
        self.layout.reveal(id, cx);
        self.set_focused(focused, cx);
        cx.notify();
    }
}
