//! Following the sessions the chats' CLIs go into: the ones they report, and
//! for Codex, which doesn't report them, the ones its session files show.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use gpui_kit::{Context, Task};
use notify::{RecursiveMode, Watcher as _};

use agentshub_core::agent::Agent;
use agentshub_core::reports::Reports;
use agentshub_core::sessions;
use agentshub_core::store::Store;

use crate::{Runtime, RuntimeEvent};

/// Codex writes its session files in bursts, so they're looked at once it
/// pauses.
const CODEX_DELAY: Duration = Duration::from_millis(500);

/// Sessions a chat's CLI went into, as one report or lookup found them.
#[derive(Clone)]
pub(crate) struct Recording {
    /// Closes once they're recorded, as are those asked for before.
    pub(crate) done: smol::channel::Receiver<()>,
    /// The last of them, once recorded. A newer recording can finish first,
    /// so what the chat's CLI was in when this was asked for is this, not the
    /// chat's `session`.
    session: Rc<Cell<Option<i64>>>,
    /// The agent's id for that session, which is the chat's before it's
    /// recorded.
    pub(crate) agent_id: String,
}

/// What follows the sessions the chats' CLIs report, and the ones Codex's
/// session files show.
pub(crate) struct Following {
    pub(crate) reports: Reports,
    codex_watcher: Option<notify::RecommendedWatcher>,
    /// What the watcher looks at: Codex's session folder, or while it doesn't
    /// exist yet, the closest folder above it.
    codex_watch: Option<(PathBuf, RecursiveMode)>,
    _report_events: Task<()>,
    _codex_events: Task<()>,
    /// The sessions whose transcript is being read, which shutdown reads
    /// itself since it can't wait for them. A session is in it once per read
    /// under way, so it stays until the last of them is done.
    pub(crate) reading: Vec<i64>,
    /// The chats whose last session is to be read once the sessions asked to
    /// be recorded are, which shutdown reads itself instead. A chat is in it
    /// once per read.
    pub(crate) reading_last: Vec<i64>,
}

impl Following {
    /// Starts taking the CLIs' reports and watching Codex's session files.
    pub(crate) fn new(cx: &mut Context<Runtime>) -> Self {
        let (reports, reported) = Reports::new();
        let report_events = cx.spawn(async move |this, cx| {
            while let Ok(report) = reported.recv().await {
                if this
                    .update(cx, |this, cx| this.read_reports(&report, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let (codex_watcher, codex_events) = watch_sessions(cx);
        Self {
            reports,
            codex_watcher,
            codex_watch: None,
            _report_events: report_events,
            _codex_events: codex_events,
            reading: Vec::new(),
            reading_last: Vec::new(),
        }
    }
}

/// Watches Codex's session files, and says whenever they change, so the
/// sessions of the Codex CLIs are looked for.
fn watch_sessions(cx: &mut Context<Runtime>) -> (Option<notify::RecommendedWatcher>, Task<()>) {
    let (codex_tx, codex_changed) = smol::channel::unbounded();
    let codex_watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        // Look at the files only when they are written. Opening a rollout
        // to find its session is an access, and scanning on that would
        // schedule the next scan forever.
        if !matches!(
            event.kind,
            notify::EventKind::Create(_) | notify::EventKind::Modify(_)
        ) {
            return;
        }
        codex_tx.try_send(()).ok();
    })
    .inspect_err(|err| eprintln!("can't watch Codex's sessions: {err}"))
    .ok();
    let codex_events = cx.spawn(async move |this, cx| {
        while codex_changed.recv().await.is_ok() {
            cx.background_executor().timer(CODEX_DELAY).await;
            while codex_changed.try_recv().is_ok() {}
            let watched = this.update(cx, |this, cx| {
                this.update_codex_watch();
                this.find_codex_sessions(cx);
            });
            if watched.is_err() {
                break;
            }
        }
    });
    (codex_watcher, codex_events)
}

impl Runtime {
    /// Points the watcher at Codex's session folder, or while it doesn't
    /// exist yet, at the closest folder above it.
    pub(crate) fn update_codex_watch(&mut self) {
        let following = &mut self.following;
        let (Some(watcher), Some(dir)) =
            (&mut following.codex_watcher, sessions::codex_sessions_dir())
        else {
            return;
        };
        let wanted = if dir.is_dir() {
            Some((dir, RecursiveMode::Recursive))
        } else {
            dir.ancestors()
                .skip(1)
                .find(|dir| dir.is_dir())
                .map(|dir| (dir.to_path_buf(), RecursiveMode::NonRecursive))
        };
        if wanted == following.codex_watch {
            return;
        }
        if let Some((path, _)) = following.codex_watch.take() {
            watcher.unwatch(&path).ok();
        }
        if let Some((path, mode)) = wanted {
            match watcher.watch(&path, mode) {
                Ok(()) => following.codex_watch = Some((path, mode)),
                Err(err) => eprintln!("can't watch {}: {err}", path.display()),
            }
        }
    }

    /// Records the sessions in the reports queued for the terminal with
    /// report name `report`.
    pub(crate) fn read_reports(&mut self, report: &str, cx: &mut Context<Self>) {
        let Some((id, agent)) = self
            .chats
            .iter()
            .find(|chat| chat.report == report)
            .map(|chat| (chat.id, chat.agent))
        else {
            return;
        };
        let sessions = reported_sessions(&mut self.following.reports, report, agent);
        self.record_sessions(id, sessions, cx);
    }

    /// Records the sessions chat `chat_id`'s CLI went into, in order, each
    /// with its CLI's id, transcript and reason, and moves the chat into the
    /// last of them. Once that is saved, reads the ones it left.
    pub(crate) fn record_sessions(
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
                cx.emit(RuntimeEvent::HistoryChanged {
                    sessions: Vec::new(),
                });
            })
            .ok();
            drop(done);
        })
        .detach();
    }

    /// Reads the session chat `id`'s CLI is in now, once the sessions it went
    /// into so far are recorded, and gives it: the last one recorded, or else
    /// the chat's. Marks the chat active when `touch` is set (see
    /// `read_sessions`).
    pub(crate) fn read_current(
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
    pub(crate) fn last_session(&self, chat_id: i64) -> impl Future<Output = Option<i64>> + use<> {
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
    pub(crate) fn read_last_session(
        &mut self,
        chat_id: i64,
        session: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let last = self.last_session(chat_id);
        self.following.reading_last.push(chat_id);
        cx.spawn(async move |this, cx| {
            let session = last.await.or(session);
            this.update(cx, |this, cx| {
                let reading_last = &mut this.following.reading_last;
                if let Some(index) = reading_last.iter().position(|each| *each == chat_id) {
                    reading_last.swap_remove(index);
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
    pub(crate) fn read_sessions(
        &mut self,
        sessions: Vec<i64>,
        touch: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let store = self.store.clone();
        self.following.reading.extend(&sessions);
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
                let reading = &mut this.following.reading;
                for id in &sessions {
                    if let Some(index) = reading.iter().position(|reading| reading == id) {
                        reading.swap_remove(index);
                    }
                }
                cx.emit(RuntimeEvent::HistoryChanged { sessions: changed });
            })
            .ok();
        })
    }

    /// Shows what reading session `id` found, once `read` says it's read.
    fn show_read(
        &mut self,
        id: i64,
        read: smol::channel::Receiver<Result<bool>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| match read.recv().await {
            Ok(Ok(changed)) => {
                let sessions = if changed { vec![id] } else { Vec::new() };
                this.update(cx, |_, cx| {
                    cx.emit(RuntimeEvent::HistoryChanged { sessions })
                })
                .ok();
            }
            Ok(Err(err)) => eprintln!("can't read session {id}: {err}"),
            Err(_) => {}
        })
        .detach();
    }

    /// Codex writes its session files as it goes, so the sessions of its
    /// terminals are looked for whenever those change.
    pub(crate) fn find_codex_sessions(&mut self, cx: &mut Context<Self>) {
        // Each chat's report name identifies this terminal, not just its row:
        // A chat can restart its terminal while the lookup runs.
        //
        // A lookup can take longer than the watcher's delay, so a newer one
        // can finish first. Marking the chats a lookup looks at with its
        // generation lets a result that has been superseded be dropped rather
        // than move a chat back to the session it has already left.
        let mut looked_at = Vec::new();
        let chats = self.chats.iter_mut().filter_map(|chat| {
            if chat.agent != Agent::Codex {
                return None;
            }
            let pid = chat.terminal.as_ref()?.terminal.read(cx).pid();
            if !pid.may_be_running() {
                return None;
            }
            looked_at.push((chat.id, chat.report.clone(), pid));
            Some(&mut chat.codex)
        });
        let Some(lookup) = self.lifecycle.look_up_codex(chats) else {
            return;
        };
        // Looking through a process's open files, and reading the first line
        // of the newest one, doesn't belong on the window's thread.
        let found = smol::unblock(move || {
            looked_at
                .into_iter()
                .filter_map(|(id, report, pid)| {
                    let (agent_id, path) = sessions::codex_session(&pid)?;
                    Some((id, report, path, agent_id))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = found.await;
            this.update(cx, |this, cx| {
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
                if !closing.is_empty() {
                    cx.emit(RuntimeEvent::ClosingAfterLookup(closing));
                }
            })
            .ok();
        })
        .detach();
    }
}

/// The sessions in a terminal's queued reports, taken out of them, in the
/// order its CLI went into them.
pub(crate) fn reported_sessions(
    reports: &mut Reports,
    key: &str,
    agent: Agent,
) -> Vec<(String, PathBuf, Option<String>)> {
    reports
        .read(key, agent)
        .into_iter()
        .map(|(agent_id, report)| (agent_id, report.transcript_path, report.reason))
        .collect()
}

/// Records the sessions a chat's CLI reported last, and reads `sessions`
/// along with the ones those reports moved the chat into or out of. One
/// transcript that can't be read doesn't stop the others.
pub(crate) fn import_chat(
    store: &Store,
    reports: &mut Reports,
    id: i64,
    agent: Agent,
    key: &str,
    mut sessions: Vec<i64>,
) -> Result<()> {
    let mut failure = None;
    for (agent_id, report) in reports.read(key, agent) {
        let recorded =
            store.record_session(id, agent, agent_id, report.transcript_path, report.reason);
        match smol::block_on(recorded) {
            Ok(recorded) => {
                sessions.push(recorded.session_id);
                if let Some((_, read)) = recorded.left
                    && let Err(err) =
                        smol::block_on(read.recv()).unwrap_or_else(|err| Err(err.into()))
                {
                    failure.get_or_insert(err);
                }
            }
            Err(err) => {
                failure.get_or_insert(err);
            }
        }
    }
    sessions.sort_unstable();
    sessions.dedup();
    for session in sessions {
        if let Err(err) = smol::block_on(store.read_transcript(session)) {
            failure.get_or_insert(err);
        }
    }
    match failure {
        Some(err) => Err(err),
        None => Ok(()),
    }
}
