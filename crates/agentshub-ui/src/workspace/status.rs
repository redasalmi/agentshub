//! Keeping the sidebar and notifications in step with what each agent does.

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
        for chat in &self.chats {
            let current = chat.current_session();
            chat.pane
                .history
                .update(cx, |history, cx| history.refresh(current, changed, cx));
        }
    }

    pub(super) fn set_status(&mut self, id: i64, status: Option<Status>, cx: &mut Context<Self>) {
        let shown = self.active == Some(id);
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) else {
            return;
        };
        let busy = |status| matches!(status, Some(Status::Working | Status::Blocked(_)));
        let was_busy = busy(chat.status);
        let alert = Alert::of(chat.status, status);
        let agent = chat.agent;
        chat.unseen = match status {
            Some(Status::Done | Status::Error) => !shown,
            // Done goes back to idle once the CLI sees the user, and a
            // cancelled turn ends in idle without a result to see.
            Some(Status::Idle) => chat.unseen,
            _ => false,
        };
        chat.status = status;
        chat.turn = Turn::after(chat.turn, was_busy, busy(status), Instant::now());
        let ended = was_busy && !busy(status);
        let took = match chat.turn {
            Some(Turn::Took(took)) if ended => Some(took),
            _ => None,
        };
        // The session is read after each turn, so the chat's title follows it.
        let read = ended.then(|| self.read_current(id, true, cx));
        // Nobody needs telling about the chat they're looking at.
        let alert = alert
            .zip(status)
            .filter(|_| !self.in_view(id, cx.active_window().is_some()));
        match alert {
            Some((alert, status)) => self.alert(id, agent, alert, status, took, read, cx),
            None => {
                if let Some(read) = read {
                    read.detach();
                }
                // Once the agent is back at work, what it needed has been
                // seen to.
                if status == Some(Status::Working) {
                    alerts::withdraw(id, cx);
                }
            }
        }
        self.sync_sidebar(cx);
    }

    /// Tells the user what chat `id`'s agent did, once its session is read,
    /// so the alert has the chat's title and quotes the agent. `read` is the
    /// read already under way, if any, which gives the session it read.
    #[expect(
        clippy::too_many_arguments,
        reason = "GPUI's context arguments count toward the limit"
    )]
    fn alert(
        &mut self,
        id: i64,
        agent: Agent,
        alert: Alert,
        status: Status,
        took: Option<Duration>,
        read: Option<Task<Option<i64>>>,
        cx: &mut Context<Self>,
    ) {
        /// A turn's last reply or tool call is never further back than this.
        const TURN_ITEMS: usize = 50;

        let hold = alerts::hold(id, cx);
        let read = read.unwrap_or_else(|| self.read_current(id, false, cx));
        let store = self.runtime.read(cx).store().clone();
        cx.spawn(async move |this, cx| {
            let session = read.await;
            let items = match session {
                Some(session) => store
                    .items(session, TURN_ITEMS)
                    .await
                    .inspect_err(|err| eprintln!("can't read session {session}: {err}"))
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            this.update(cx, |this, cx| {
                if this.in_view(id, cx.active_window().is_some()) {
                    return;
                }
                let mut body = this
                    .sidebar
                    .read(cx)
                    .describe(id)
                    .unwrap_or_else(|| "A chat in Agents Hub".into());
                if let Some(details) = alerts::details(alert, &items) {
                    body.push('\n');
                    body.push_str(&details);
                }
                let mut summary = sidebar::activity(agent, Some(Some(status)));
                if let Some(took) = took {
                    summary = format!("{summary} · {}", duration(took));
                }
                alerts::alert(id, hold, alert, summary, body, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Whether the user can see chat `id`: it's shown, nothing covers it and
    /// the window has the focus.
    fn in_view(&self, id: i64, focused: bool) -> bool {
        self.active == Some(id) && !self.settings_shown && !self.hidden && focused
    }

    /// Takes back the notification about the shown chat once the user can
    /// see it, as when the window takes the focus or the settings close.
    /// `focused` is passed in because, on X11, the window's activation is
    /// reported before `cx.active_window()` catches up with it.
    pub fn withdraw_seen(&self, focused: bool, cx: &mut App) {
        if let Some(id) = self.active
            && self.in_view(id, focused)
        {
            alerts::withdraw(id, cx);
        }
    }

    /// Marks the chat that is shown and what each running one is doing.
    pub(super) fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        let running = self
            .chats
            .iter()
            .filter(|chat| chat.running())
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
        let active = self.active;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_chats(running, active, cx));
        self.tick_turn(cx);
    }

    /// Keeps the shown chat's turn up to date while it's under way, and only
    /// then.
    fn tick_turn(&mut self, cx: &mut Context<Self>) {
        let under_way = self.chats.iter().any(|chat| {
            self.active == Some(chat.id)
                && chat.running()
                && matches!(chat.turn, Some(Turn::Since(_)))
        });
        if !under_way {
            self.turn_tick = None;
        } else if self.turn_tick.is_none() {
            self.turn_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TURN_INTERVAL).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }
}
