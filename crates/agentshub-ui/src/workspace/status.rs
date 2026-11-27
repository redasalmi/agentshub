//! Keeping the sidebar and the chats' histories in step with what each agent
//! does.

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
        if self.runtime.read(cx).catalog().unsaved() == 0 {
            let listed = self.runtime.read(cx).store().projects();
            let listing = cx.spawn(async move |this, cx| {
                let listed = listed.await;
                this.update(cx, |this, cx| {
                    // A change shown since is listed once it's saved.
                    if this.runtime.read(cx).catalog().unsaved() > 0 {
                        return;
                    }
                    let shell = this.shell_cwd(cx);
                    match listed {
                        Ok(projects) => {
                            this.runtime
                                .update(cx, |runtime, _| runtime.catalog_mut().listed(projects));
                            this.sync_catalog(cx);
                        }
                        Err(err) => eprintln!("can't list the projects: {err}"),
                    }
                    if let Some(id) = this
                        .runtime
                        .update(cx, |runtime, _| runtime.catalog_mut().take_selected())
                    {
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.select(id, cx));
                    }
                    // Which shell shows depends on the projects' chats, and a
                    // new chat shown before they list it got its project's
                    // shell meanwhile.
                    if this.shell_cwd(cx) != shell {
                        this.sync_terminals(cx);
                        cx.notify();
                    }
                })
                .ok();
            });
            self.runtime
                .update(cx, |runtime, _| runtime.catalog_mut().listing(listing));
        }
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
    /// says whether the window has the focus, and the settings or the tray
    /// may cover the chat. `focused` is passed in because, on X11, the
    /// window's activation is reported before `cx.active_window()` catches up
    /// with it.
    pub fn set_focused(&self, focused: bool, cx: &mut App) {
        let seen = focused && !self.settings_shown && !self.hidden;
        self.apply(Intent::SetFocused(seen), cx);
    }

    /// Marks the chat that is shown and what each running one is doing.
    pub(super) fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        let runtime = self.runtime.read(cx);
        let running = runtime
            .open_chats()
            .filter(|chat| chat.running)
            .map(|chat| {
                // The sidebar shows a finished CLI as done until it is seen.
                let status = match chat.status {
                    Some(Status::Idle | Status::Done) if chat.unseen => Some(Status::Done),
                    Some(Status::Done) => Some(Status::Idle),
                    status => status,
                };
                (chat.id, status)
            })
            .collect();
        let active = runtime.active();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_chats(running, active, cx));
    }
}
