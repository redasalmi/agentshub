//! Following the sessions a chat's CLI goes into.

use super::*;

impl Workspace {
    pub(super) fn read_reports(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some((id, agent)) = self
            .chats
            .iter()
            .find(|chat| chat.report == key)
            .map(|chat| (chat.id, chat.agent))
        else {
            return;
        };
        let sessions = reported_sessions(&mut self.reports, key, agent);
        self.record_sessions(id, sessions, cx);
    }

    /// Records the sessions a chat's CLI went into, in order, each with its
    /// CLI's id, transcript and reason, and once that is saved, reads the ones
    /// it left.
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
        let recorded: Vec<_> = sessions
            .into_iter()
            .map(|(agent_id, path, reason)| {
                let recorded =
                    self.store
                        .record_session(chat_id, agent, agent_id.clone(), path, reason);
                async move { (agent_id, recorded.await) }
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut answers = Vec::new();
            for answer in recorded {
                answers.push(answer.await);
            }
            this.update(cx, |this, cx| {
                let mut session = None;
                for (agent_id, recorded) in answers {
                    match recorded {
                        Ok(recorded) => {
                            session = Some(recorded.session_id);
                            if let Some((left, read)) = recorded.left {
                                this.show_read(left, read, cx);
                            }
                        }
                        Err(err) => eprintln!("can't record session {agent_id}: {err}"),
                    }
                }
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

    /// Reads the session chat `id`'s CLI is in now, once the sessions it went
    /// into so far are recorded, and gives it. Marks the chat active when `touch` is
    /// set (see `read_sessions`).
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
        cx.spawn(async move |this, cx| {
            let session = match recording {
                Some(recording) => {
                    recording.done.recv().await.ok();
                    recording.session.get().or(current)
                }
                None => current,
            }?;
            // The turn was in it, even if the chat has closed or restarted
            // since.
            let read = this
                .update(cx, |this, cx| this.read_sessions(vec![session], touch, cx))
                .ok()?;
            read.await;
            Some(session)
        })
    }

    /// The session a chat's CLI is in, as listed once the sessions asked to
    /// be recorded so far are, which the chat may not know of yet: its last.
    pub(super) fn last_session(&self, chat_id: i64) -> impl Future<Output = Option<i64>> + use<> {
        let sessions = self.store.sessions(chat_id);
        async move {
            let mut sessions = sessions
                .await
                .inspect_err(|err| eprintln!("can't list the sessions of chat {chat_id}: {err}"))
                .ok()?;
            sessions.pop().map(|session| session.id)
        }
    }

    /// Reads the session a chat's CLI was in: its last, or else `session`.
    pub(super) fn read_last_session(
        &mut self,
        chat_id: i64,
        session: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let last = self.last_session(chat_id);
        self.reading_last.push(chat_id);
        cx.spawn(async move |this, cx| {
            let session = last.await.or(session);
            this.update(cx, |this, cx| {
                if let Some(index) = this.reading_last.iter().position(|each| *each == chat_id) {
                    this.reading_last.swap_remove(index);
                }
                this.read_sessions(session.into_iter().collect(), false, cx)
                    .detach()
            })
            .ok();
        })
        .detach();
    }

    /// Reads sessions' transcripts on a blocking thread, then shows what
    /// changed. Marks their chats active when `touch` is set, as a finished
    /// turn does but browsing history doesn't.
    pub(super) fn read_sessions(
        &mut self,
        sessions: Vec<i64>,
        touch: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let store = self.store.clone();
        self.reading.extend(&sessions);
        cx.spawn(async move |this, cx| {
            if sessions.is_empty() {
                return;
            }
            let mut changed = Vec::new();
            for &id in &sessions {
                match store.read_transcript(id).await {
                    Ok(read) => {
                        if touch && let Err(err) = store.touch_session(id).await {
                            eprintln!("can't touch session {id}: {err}");
                        }
                        if read {
                            changed.push(id);
                        }
                    }
                    Err(err) => {
                        eprintln!("can't read session {id}: {err}");
                    }
                }
            }
            this.update(cx, |this, cx| {
                for id in &sessions {
                    if let Some(index) = this.reading.iter().position(|reading| reading == id) {
                        this.reading.swap_remove(index);
                    }
                }
                this.refresh_after(&changed, cx);
            })
            .ok();
        })
    }

    /// Shows what reading session `id` found, once `read` says it's read.
    fn show_read(
        &mut self,
        id: i64,
        read: smol::channel::Receiver<anyhow::Result<bool>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| match read.recv().await {
            Ok(Ok(changed)) => {
                let changed = if changed { vec![id] } else { Vec::new() };
                this.update(cx, |this, cx| this.refresh_after(&changed, cx))
                    .ok();
            }
            Ok(Err(err)) => eprintln!("can't read session {id}: {err}"),
            Err(_) => {}
        })
        .detach();
    }
}
