//! Starting, stopping and restarting the CLI behind a chat.

use super::*;

impl Workspace {
    /// Starts a chat's CLI, resuming `session` if given. The chat stays hidden
    /// until it is activated.
    pub(super) fn start_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        session: Option<&store::Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let report = uuid::Uuid::new_v4().to_string();
        let colors = appearance::terminal_colors(cx);
        let mut all_args = self.runtime.read(cx).reports().args(agent);
        if let Some(session) = session {
            all_args.extend(agent.resume_args(&session.agent_id, &session.transcript_path));
        }
        let report_path = self.runtime.read(cx).reports().path(&report);
        let env = [(reports::ENV, report_path.as_os_str())];
        let terminal = match Pty::spawn(agent.program(), &all_args, &env, cwd, colors) {
            Ok(pty) => {
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
                let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
                // Hidden terminals don't need to build frames.
                terminal.read(cx).set_visible(false);
                let exited = cx.subscribe(
                    &terminal,
                    move |this, _, &Exited { success }: &Exited, cx| {
                        this.chat_exited(id, success, cx)
                    },
                );
                let status = cx.subscribe(
                    &terminal,
                    move |this, _, StatusChanged(status): &StatusChanged, cx| {
                        this.set_status(id, *status, cx)
                    },
                );
                Ok(ChatTerminal {
                    terminal,
                    view,
                    exited: false,
                    _exited: exited,
                    _status: status,
                })
            }
            Err(err) => {
                let message = format!("Couldn't start {}: {err}", agent.program());
                window.push_notification(message.clone(), cx);
                Err(SharedString::from(message))
            }
        };
        let started = terminal.is_ok();
        self.push_chat(id, agent, cwd, report, terminal, session, cx);
        started
    }

    /// Adds a chat to the view, with the history of the sessions it has: with
    /// its terminal, or with why its CLI didn't start.
    #[expect(
        clippy::too_many_arguments,
        reason = "GPUI's context arguments count toward the limit"
    )]
    fn push_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        report: String,
        terminal: Result<ChatTerminal, SharedString>,
        session: Option<&store::Session>,
        cx: &mut Context<Self>,
    ) -> &mut Chat {
        let (terminal, message) = match terminal {
            Ok(terminal) => (Some(terminal), None),
            Err(message) => (None, Some(message)),
        };
        // Without a running CLI, even its last session belongs in history.
        let current = terminal.as_ref().and(session.map(|session| session.id));
        let transcript = session.map(|session| session.transcript_path.clone());
        let store = self.runtime.read(cx).store().clone();
        let history = cx.new(|cx| {
            // Without a terminal, the history is all there is to show.
            let mut history = History::new(Rc::new(store), id, terminal.is_none());
            history.refresh(current, &[], cx);
            history
        });
        self.chats.push(Chat {
            runtime: ChatRuntime {
                id,
                agent,
                cwd: cwd.to_path_buf(),
                report,
                message,
                session: session.map(|session| session.id),
                transcript,
                recording: None,
                status: None,
                turn: None,
                unseen: false,
                codex: Codex::default(),
                notice: None,
                fresh_on_restart: false,
            },
            pane: ChatPane { terminal, history },
        });
        self.chats.last_mut().expect("a chat was just pushed")
    }

    /// Whether another chat's CLI is running in a session.
    pub(super) fn live_elsewhere(
        &self,
        chat_id: i64,
        agent: Agent,
        session: &store::Session,
        cx: &App,
    ) -> bool {
        self.chats.iter().any(|chat| {
            chat.id != chat_id
                && (chat.current_session() == Some(session.id)
                    // One its CLI reported is in it before it's recorded.
                    || chat.running()
                        && chat.agent == agent
                        && chat.recording.as_ref().is_some_and(|recording| {
                            !recording.done.is_closed() && recording.agent_id == session.agent_id
                        }))
        }) || self
            .runtime
            .read(cx)
            .lifecycle()
            .stopped_elsewhere(chat_id, session.id)
    }

    /// Shows a chat whose CLI wasn't started, with the reason above the
    /// history. Restarting starts a new session rather than resuming the one
    /// that couldn't start.
    pub(super) fn block_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        session: Option<&store::Session>,
        message: SharedString,
        cx: &mut Context<Self>,
    ) {
        let report = uuid::Uuid::new_v4().to_string();
        self.push_chat(id, agent, cwd, report, Err(message), session, cx)
            .fresh_on_restart = true;
        self.sync_sidebar(cx);
        cx.notify();
    }

    /// Restarts a chat's CLI, starting a new session when the chat couldn't
    /// have resumed its last one.
    pub(super) fn restart(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = self
            .chats
            .iter()
            .find(|chat| chat.id == id && chat.fresh_on_restart)
            .map(|chat| (chat.agent, chat.cwd.clone()));
        // The CLI goes, so what it is in is recorded while it is still there.
        let codex = self.codex_session(id, cx);
        self.record_running(id, codex, cx);
        let session = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(|chat| chat.session);
        if let Some(reaped) = self.stop_chat(id, cx) {
            self.track_stopping(id, reaped, session, cx);
        }
        if let Some((agent, cwd)) = fresh {
            if self.start_chat(id, agent, &cwd, None, window, cx) {
                self.activate(id, window, cx);
            }
            self.refresh(cx);
        } else {
            self.open_chat(id, window, cx);
        }
    }

    /// Archives the last session. A CLI the user quit closes its chat;
    /// otherwise the terminal's exit diagnostics stay until the user restarts
    /// or deletes the chat.
    fn chat_exited(&mut self, id: i64, success: bool, cx: &mut Context<Self>) {
        let Some(index) = self.chats.iter().position(|chat| chat.id == id) else {
            return;
        };
        // A report can still be queued when the CLI's exit is handled. It
        // says which session the final read follows, so it is read before the
        // last session is imported.
        let report = self.chats[index].report.clone();
        self.read_reports(&report, cx);
        let chat = &mut self.chats[index];
        // A CLI that quits by itself without failing, as on a startup error,
        // keeps its terminal too, since only typing quits it otherwise.
        let quit = success
            && chat.pane.terminal.as_ref().is_some_and(|terminal| {
                terminal.terminal.read(cx).input() != terminal::Input::Untouched
            });
        if quit {
            // A running Codex lookup may still find the session the CLI was
            // in, and nothing else would record it.
            if !chat.codex.close_after_lookup() {
                self.close_chat(id, cx);
            }
            return;
        }
        if let Some(terminal) = &mut chat.pane.terminal {
            terminal.exited = true;
        }
        chat.message = Some(
            format!(
                "{} exited. Its terminal output is kept below.",
                chat.agent.display_name()
            )
            .into(),
        );
        chat.status = None;
        let session = chat.session;
        chat.pane
            .history
            .update(cx, |history, cx| history.refresh(None, &[], cx));
        self.sync_sidebar(cx);
        cx.notify();
        self.runtime
            .update(cx, |runtime, cx| runtime.read_last_session(id, session, cx));
    }

    /// Closes a chat whose CLI the user quit, and reads its last session.
    pub(super) fn close_chat(&mut self, id: i64, cx: &mut Context<Self>) {
        let session = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(|chat| chat.session);
        match self.stop_chat(id, cx) {
            Some(reaped) => self.track_stopping(id, reaped, session, cx),
            None => self
                .runtime
                .update(cx, |runtime, cx| runtime.read_last_session(id, session, cx)),
        }
    }

    /// Kills the chat's CLI, if it has one, and closes the chat. The channel
    /// closes once the CLI has been reaped.
    pub(super) fn stop_chat(
        &mut self,
        id: i64,
        cx: &mut Context<Self>,
    ) -> Option<smol::channel::Receiver<()>> {
        let index = self.chats.iter().position(|chat| chat.id == id)?;
        let chat = self.chats.remove(index);
        let reaped = chat
            .pane
            .terminal
            .map(|terminal| terminal.terminal.read(cx).shutdown());
        if self.active == Some(id) {
            self.active = None;
        }
        self.sync_terminals(cx);
        self.sync_sidebar(cx);
        cx.notify();
        reaped
    }

    /// Whether a chat's CLI can be stopped without losing anything the user
    /// would miss, shown or not: it's at rest, has nothing unseen, holds no
    /// draft, and can resume its session. `kept` says whether its transcript
    /// exists, and `last_prompt` is its session's (see `Store::last_prompt`).
    pub(super) fn at_rest(
        chat: &Chat,
        kept: bool,
        last_prompt: &Result<(bool, Option<i64>)>,
        cx: &App,
    ) -> bool {
        !chat.unseen
            && matches!(chat.status, Some(Status::Idle | Status::Done))
            && chat.pane.terminal.as_ref().is_some_and(|terminal| {
                !terminal.exited
                    && terminal
                        .terminal
                        .read(cx)
                        .input()
                        .sent(|| last_prompt.as_ref().ok().and_then(|(_, at)| *at))
            })
            // Without its transcript, a session with a prompt can't be resumed,
            // and the CLI holds the only copy of it. Claude deletes transcripts
            // that went untouched for 30 days.
            && (kept || matches!(last_prompt, Ok((false, _))))
    }

    /// The session a chat's Codex CLI is in, looked for among the files its
    /// process keeps open, which only that process can tell. `None` for the
    /// other agents, which report their sessions. That's a few reads under
    /// /proc, which a chat stopping now and then can afford on this thread.
    fn codex_session(&self, id: i64, cx: &App) -> Option<(String, PathBuf)> {
        let chat = self.chats.iter().find(|chat| chat.id == id)?;
        let terminal = chat
            .pane
            .terminal
            .as_ref()
            .filter(|_| chat.agent == Agent::Codex)?;
        sessions::codex_session(&terminal.terminal.read(cx).pid())
    }

    /// Records what a chat's CLI is in before it is stopped: the sessions in
    /// its queued reports, and `codex` where its open files told the one it
    /// is in. Everything that kills a CLI goes through here first, or the
    /// chat opens again on a session it had already left, and the one it was
    /// really in is never recorded, nor its transcript ever read.
    fn record_running(
        &mut self,
        id: i64,
        codex: Option<(String, PathBuf)>,
        cx: &mut Context<Self>,
    ) {
        let report = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .map(|chat| chat.report.clone());
        // A queued report can say the CLI went into another session.
        if let Some(report) = report {
            self.read_reports(&report, cx);
        }
        if let Some((agent_id, path)) = codex {
            self.record_sessions(id, vec![(agent_id, path, None)], cx);
        }
    }

    /// Stops a chat's CLI after recording the session it's in, which opening
    /// the chat again resumes, and reads that session once the CLI is gone.
    pub(super) fn suspend(&mut self, id: i64, cx: &mut Context<Self>) {
        let codex = self.codex_session(id, cx);
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) else {
            return;
        };
        // A Codex that exited, or is exiting, while a lookup was under way
        // has no open files left to look through, so only that lookup can
        // find the session it was in. The chat stops once it has recorded it,
        // as when the user quits the CLI.
        if codex.is_none() && chat.codex.close_after_lookup() {
            return;
        }
        self.record_running(id, codex, cx);
        let Some(session) = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .map(|chat| chat.session)
        else {
            return;
        };
        let Some(reaped) = self.stop_chat(id, cx) else {
            return;
        };
        self.track_stopping(id, reaped, session, cx);
    }

    /// Keeps chat `id` from starting again, and its session from being
    /// resumed elsewhere, until its stopped CLI has been reaped, then reads
    /// the session it was in: its last, or else `session`.
    fn track_stopping(
        &mut self,
        id: i64,
        reaped: Reaped,
        session: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        self.runtime.update(cx, |runtime, _| {
            runtime.lifecycle_mut().stop(id, reaped.clone(), session)
        });
        let last = self.runtime.read(cx).last_session(id);
        cx.spawn(async move |this, cx| {
            let session = last.await.or(session);
            this.update(cx, |this, cx| {
                this.runtime.update(cx, |runtime, _| {
                    runtime.lifecycle_mut().stopped_in(id, &reaped, session)
                })
            })
            .ok();
            // What the CLI writes as it exits is part of the session.
            reaped.recv().await.ok();
            this.update(cx, |this, cx| {
                this.runtime.update(cx, |runtime, cx| {
                    runtime.lifecycle_mut().reaped(id, &reaped);
                    runtime
                        .read_sessions(session.into_iter().collect(), false, cx)
                        .detach();
                });
            })
            .ok();
        })
        .detach();
    }

    /// Records the sessions in the reports queued for the terminal with
    /// report name `report`.
    pub(super) fn read_reports(&mut self, report: &str, cx: &mut Context<Self>) {
        let Some((id, agent)) = self
            .chats
            .iter()
            .find(|chat| chat.report == report)
            .map(|chat| (chat.id, chat.agent))
        else {
            return;
        };
        let sessions = self
            .runtime
            .update(cx, |runtime, _| runtime.read_reports(agent, report));
        self.record_sessions(id, sessions, cx);
    }

    /// Records the sessions a chat's CLI went into, in order (see
    /// `Runtime::record_sessions`), and moves the chat into the last of them.
    pub(super) fn record_sessions(
        &mut self,
        chat_id: i64,
        sessions: Vec<(String, PathBuf, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id) else {
            return;
        };
        let Some((agent_id, path, _)) = sessions.last() else {
            return;
        };
        chat.transcript = Some(path.clone());
        // Sessions are recorded in the order they're asked for, so once the
        // last ones are, so are those before.
        let (done, receiver) = smol::channel::bounded::<()>(1);
        let recording = Recording {
            done: receiver,
            session: Rc::default(),
            agent_id: agent_id.clone(),
        };
        chat.recording = Some(recording.clone());
        let (agent, report) = (chat.agent, chat.report.clone());
        let recorded = self.runtime.update(cx, |runtime, cx| {
            runtime.record_sessions(chat_id, agent, sessions, cx)
        });
        cx.spawn(async move |this, cx| {
            let session = recorded.await;
            this.update(cx, |this, cx| {
                let Some(session) = session else {
                    return;
                };
                recording.session.set(Some(session));
                // A newer terminal of the chat's is in a session of its own,
                // and sessions its CLI reported since are newer than these.
                if let Some(chat) = this.chats.iter_mut().find(|chat| {
                    chat.id == chat_id
                        && chat.report == report
                        && chat
                            .recording
                            .as_ref()
                            .is_some_and(|last| Rc::ptr_eq(&last.session, &recording.session))
                }) {
                    chat.session = Some(session);
                }
                // Reports arrive in bursts, and each one used to reload the
                // sidebar.
                this.refresh(cx);
            })
            .ok();
            drop(done);
        })
        .detach();
    }

    /// Reads the session chat `id`'s CLI is in now, and gives it (see
    /// `Runtime::read_current`).
    pub(super) fn read_current(
        &mut self,
        id: i64,
        touch: bool,
        cx: &mut Context<Self>,
    ) -> Task<Option<i64>> {
        let Some(chat) = self.chats.iter().find(|chat| chat.id == id) else {
            return Task::ready(None);
        };
        let (recording, current) = (chat.recording.clone(), chat.session);
        self.runtime.update(cx, |runtime, cx| {
            runtime.read_current(recording, current, touch, cx)
        })
    }

    /// Codex writes its session files as it goes, so the sessions of its
    /// terminals are looked for whenever those change.
    pub(super) fn find_codex_sessions(&mut self, cx: &mut Context<Self>) {
        let lookup = self.runtime.update(cx, |runtime, cx| {
            let chats = self.chats.iter_mut().filter_map(|chat| {
                if chat.agent != Agent::Codex {
                    return None;
                }
                let pid = chat.pane.terminal.as_ref()?.terminal.read(cx).pid();
                if !pid.may_be_running() {
                    return None;
                }
                Some((chat.id, chat.report.clone(), pid, &mut chat.codex))
            });
            runtime.find_codex_sessions(chats)
        });
        let Some((lookup, found)) = lookup else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let found = found.await;
            let Ok(closing) = this.update(cx, |this, cx| {
                for (id, report, path, agent_id) in found {
                    // A chat with this id now is another chat, or the same
                    // one with a newer terminal: either way the result is
                    // about a process that has gone. The same goes for a
                    // lookup that a newer one has already replaced.
                    let current = this
                        .chats
                        .iter()
                        .find(|chat| chat.id == id && chat.report == report)
                        .is_some_and(|chat| {
                            chat.codex.looked_up_by(lookup)
                                && chat.transcript.as_ref() != Some(&path)
                        });
                    if !current {
                        continue;
                    }
                    this.record_sessions(id, vec![(agent_id, path, None)], cx);
                }
                let mut closing = Vec::new();
                for chat in &mut this.chats {
                    if chat.codex.looked_up(lookup) {
                        closing.push(chat.id);
                    }
                }
                closing
            }) else {
                return;
            };
            if closing.is_empty() {
                return;
            }
            // One closed while its CLI was exiting may not be reaped yet, so
            // its session is read once it is, and nothing else runs in that
            // session until then. The shown chat takes the keyboard with it,
            // which only its window can give back.
            let closed = cx.update(|cx| {
                cx.with_window(this.entity_id(), |window, cx| {
                    this.update(cx, |this, cx| this.close_chats(closing.clone(), window, cx))
                })
            });
            // Closed to the tray, the window has no keyboard to give back, and
            // opening it again focuses the shown chat.
            if !matches!(closed, Some(Ok(()))) {
                this.update(cx, |this, cx| {
                    for id in closing {
                        this.suspend(id, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn activate(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) {
            chat.unseen = false;
        }
        alerts::withdraw(id, cx);
        self.active = Some(id);
        self.runtime
            .update(cx, |runtime, _| runtime.lifecycle_mut().shown(id));
        self.settings_shown = false;
        self.focus_chat(window, cx);
        self.sync_terminals(cx);
        self.sync_sidebar(cx);
        // Showing a chat, even the one already shown, brings its project back
        // into the rail's selection, so the chat list and New Chat follow it.
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.reveal(id, cx));
        cx.notify();
    }
}
