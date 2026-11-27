//! The chats' CLIs and their terminals: starting, stopping and restarting
//! them, what they report doing, and telling the user about it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui_kit::{App, AppContext as _, Context, Entity, SharedString, Subscription, Task};

use agentshub_app::alerts::{self, Alert};
use agentshub_core::agent::Agent;
use agentshub_core::format::{Turn, activity, duration};
use agentshub_core::lifecycle::{Codex, Reaped};
use agentshub_core::reports;
use agentshub_core::sessions;
use agentshub_core::status::Status;
use agentshub_core::store::{self, Change};
use agentshub_core::worktree::{self, Worktree};

use crate::sessions::{Recording, import_chat};
use crate::terminal::{self, Colors, Exited, Pty, StatusChanged, Terminal};
use crate::{Runtime, RuntimeEvent};

/// How often the time the shown chat's prompt has taken so far is brought up
/// to date.
const TURN_INTERVAL: Duration = Duration::from_secs(1);

/// Said above a chat whose last session couldn't be resumed, and kept in that
/// session's history.
const TRANSCRIPT_GONE: &str = "This session's transcript is gone, so it couldn't be resumed. \
                               Its saved history is still available here.";

/// What a view shows of an open chat.
pub struct ChatState<'a> {
    pub id: i64,
    pub agent: Agent,
    /// Its project's folder, or its worktree's, which its CLI runs in.
    pub cwd: &'a Path,
    /// What the CLI last reported doing, if it reports that at all.
    pub status: Option<Status>,
    /// How long the CLI has been on the last prompt, or took on it.
    pub turn: Option<Turn>,
    /// Whether the CLI finished while another chat was shown, and hasn't been
    /// looked at since.
    pub unseen: bool,
    /// Whether its CLI runs.
    pub running: bool,
    /// Startup failure or exit message, shown alongside the saved history.
    pub message: Option<&'a SharedString>,
    /// Something about the session to show above the terminal, without a
    /// restart button.
    pub notice: Option<&'a SharedString>,
}

/// A chat's backend state: what its CLI is doing and which session it is in.
pub(crate) struct ChatRuntime {
    pub(crate) id: i64,
    pub(crate) agent: Agent,
    /// Its project's folder, which its CLI runs in.
    cwd: PathBuf,
    /// The name of the file its CLI reports its sessions to.
    pub(crate) report: String,
    /// Startup failure or exit message, shown alongside the saved history.
    message: Option<SharedString>,
    /// The session the CLI is in, once known. It changes when the CLI
    /// switches to another session, as on `/clear`.
    pub(crate) session: Option<i64>,
    /// That session's transcript.
    pub(crate) transcript: Option<PathBuf>,
    /// The sessions the CLI last went into, being recorded.
    pub(crate) recording: Option<Recording>,
    /// What the CLI last reported doing, if it reports that at all.
    status: Option<Status>,
    /// How long the CLI has been on the last prompt, or took on it.
    turn: Option<Turn>,
    /// Whether the CLI finished while another chat was shown, and hasn't been
    /// looked at since.
    unseen: bool,
    /// The lookups of the session its CLI is in, if it's Codex.
    pub(crate) codex: Codex,
    /// Something about the session to show above the terminal, without a
    /// restart button.
    notice: Option<SharedString>,
    /// Whether restarting starts a new session instead of resuming the last.
    fresh_on_restart: bool,
    /// Its CLI's terminal, unless the CLI couldn't start.
    pub(crate) terminal: Option<ChatTerminal>,
}

/// A chat's CLI, running or exited, and what the runtime hears from it.
pub(crate) struct ChatTerminal {
    pub(crate) terminal: Entity<Terminal>,
    exited: bool,
    _exited: Subscription,
    _status: Subscription,
}

impl ChatRuntime {
    fn running(&self) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|terminal| !terminal.exited)
    }

    fn current_session(&self) -> Option<i64> {
        self.session.filter(|_| self.running())
    }

    fn state(&self) -> ChatState<'_> {
        ChatState {
            id: self.id,
            agent: self.agent,
            cwd: &self.cwd,
            status: self.status,
            turn: self.turn,
            unseen: self.unseen,
            running: self.running(),
            message: self.message.as_ref(),
            notice: self.notice.as_ref(),
        }
    }
}

/// Takes the worktree out of `job` and removes it, unless it was taken.
#[doc(hidden)]
pub fn take_and_remove(
    job: &Mutex<Option<(Worktree, PathBuf)>>,
) -> Option<Result<worktree::Removal>> {
    agentshub_core::lifecycle::take_and_remove(job, worktree::remove)
}

/// Whether a chat's CLI can be stopped without losing anything the user
/// would miss, shown or not: it's at rest, has nothing unseen, holds no
/// draft, and can resume its session. `kept` says whether its transcript
/// exists, and `last_prompt` is its session's (see `Store::last_prompt`).
fn at_rest(
    chat: &ChatRuntime,
    kept: bool,
    last_prompt: &Result<(bool, Option<i64>)>,
    cx: &App,
) -> bool {
    !chat.unseen
        && matches!(chat.status, Some(Status::Idle | Status::Done))
        && chat.terminal.as_ref().is_some_and(|terminal| {
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

impl Runtime {
    /// Chat `id`, if it is open.
    pub fn chat(&self, id: i64) -> Option<ChatState<'_>> {
        self.chats
            .iter()
            .find(|chat| chat.id == id)
            .map(ChatRuntime::state)
    }

    /// The open chats, including exited terminals and history whose CLI
    /// couldn't start, in the order they opened.
    pub fn open_chats(&self) -> impl Iterator<Item = ChatState<'_>> {
        self.chats.iter().map(ChatRuntime::state)
    }

    /// The chat that is shown.
    pub fn active(&self) -> Option<i64> {
        self.active
    }

    /// Chat `id`'s terminal, running or exited, unless its CLI didn't start.
    pub fn terminal(&self, id: i64) -> Option<Entity<Terminal>> {
        self.chats
            .iter()
            .find(|chat| chat.id == id)?
            .terminal
            .as_ref()
            .map(|terminal| terminal.terminal.clone())
    }

    /// The session chat `id`'s CLI is in, once known, while the CLI runs.
    pub fn current_session(&self, id: i64) -> Option<i64> {
        self.chats
            .iter()
            .find(|chat| chat.id == id)?
            .current_session()
    }

    /// Paints every chat's terminal in `colors`, and the ones that start from
    /// now on.
    pub fn set_colors(&mut self, colors: Colors, cx: &App) {
        self.colors = Some(colors);
        for chat in &self.chats {
            if let Some(terminal) = &chat.terminal {
                terminal.terminal.read(cx).set_colors(colors);
            }
        }
    }

    /// Whether a chat, or its project, is archived.
    fn is_archived(&self, id: i64) -> bool {
        self.projects().iter().any(|project| {
            project.chats.iter().any(|chat| {
                chat.id == id && (chat.archived_at.is_some() || project.archived_at.is_some())
            })
        })
    }

    /// Whether chat `id`'s project is being removed.
    fn is_removing(&self, id: i64) -> bool {
        self.projects().iter().any(|project| {
            self.lifecycle.is_removing_project(project.id)
                && project.chats.iter().any(|chat| chat.id == id)
        })
    }

    /// Which chat `id` is, for telling the user about it away from the
    /// sidebar: its title and its project's name.
    fn describe(&self, id: i64) -> Option<String> {
        self.projects().iter().find_map(|project| {
            let chat = project.chats.iter().find(|chat| chat.id == id)?;
            Some(match &chat.title {
                Some(title) => format!("{title} · {}", project.name),
                None => format!("New chat in {}", project.name),
            })
        })
    }

    /// Shows a chat, starting its CLI unless it runs. An archived chat, or
    /// one in an archived project, is brought back first.
    #[expect(
        clippy::too_many_lines,
        reason = "the states a chat can be in when opened, checked in order"
    )]
    pub(crate) fn open_chat(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.lifecycle.is_deleting(id) || self.is_removing(id) {
            return;
        }
        if self.is_archived(id) {
            self.change(Change::RestoreChat(id), cx);
        }
        // Reading a chat's last session takes long enough for another chat
        // to be asked for meanwhile. This is the request that a completion
        // compares itself with.
        let request = self.lifecycle.request(id);
        if self.chats.iter().any(|chat| chat.id == id) {
            self.activate(id, cx);
            return;
        }
        // A CLI that was stopped to free what it held opens again once it's
        // gone, so the two never run in one session.
        // A window that closed meanwhile doesn't open it.
        let window_closes = self.window_closes;
        if let Some(reaped) = self.lifecycle.reaping(id) {
            cx.spawn(async move |this, cx| {
                reaped.recv().await.ok();
                this.update(cx, |this, cx| {
                    if this.window_closes == window_closes && this.lifecycle.is_requested(id) {
                        this.open_chat(id, cx);
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        // Clicks that arrive while the session is read would start a second
        // CLI in the same chat. A read that was cancelled starts it after all.
        if !self.lifecycle.open(id) {
            return;
        }

        // The last session decides how the CLI starts, so it's read first: the
        // CLI may have gone on without agentshub. Transcripts can be large
        // enough that this can't happen while the window waits.
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let read = async {
                let mut sessions = store.sessions(id).await?;
                let paths: Vec<PathBuf> = sessions
                    .iter()
                    .map(|session| session.transcript_path.clone())
                    .collect();
                let exist: Vec<bool> =
                    smol::unblock(move || paths.iter().map(|path| path.exists()).collect()).await;
                let mut exist = exist.into_iter();
                let last_exists = exist.next_back().unwrap_or(false);
                if let Some(last) = sessions.pop()
                    && last_exists
                {
                    store.read_transcript(last.id).await?;
                    // Opening a chat is the chat being active, which orders the
                    // sidebar. Reading its history must not do the same.
                    store.touch_session(last.id).await?;
                }
                // The history hides a session without a prompt, so it is never
                // read again there, even once a newer parser would find one.
                for (earlier, exists) in sessions.iter().zip(exist) {
                    if !earlier.prompted
                        && exists
                        && let Err(err) = store.read_transcript(earlier.id).await
                    {
                        eprintln!("can't read session {}: {err}", earlier.id);
                    }
                }
                anyhow::Ok(())
            }
            .await;
            if let Err(err) = read {
                eprintln!("can't read the last session of chat {id}: {err}");
            }
            // The chat as it is once read: it may have been deleted meanwhile,
            // and its last session found prompted.
            let opened = async {
                let chat = store.chat(id).await?;
                let sessions = store.sessions(id).await?;
                let path = sessions.last().map(|last| last.transcript_path.clone());
                let last_exists =
                    smol::unblock(move || path.is_some_and(|path| path.exists())).await;
                anyhow::Ok((chat, sessions, last_exists))
            }
            .await;
            this.update(cx, |this, cx| {
                // The window closed meanwhile, so the chat isn't started, but
                // it can be opened again.
                if this.window_closes != window_closes {
                    this.lifecycle.opened(id);
                    return;
                }
                // The chats may have been closed meanwhile.
                if !this.lifecycle.opened(id) {
                    return;
                }
                // The chat may have been deleted, or opened elsewhere, meanwhile,
                // or its project be being removed.
                if this.chats.iter().any(|chat| chat.id == id) || this.is_removing(id) {
                    return;
                }
                let (agent, cwd, sessions, last_exists) = match opened {
                    Ok((Some((agent, cwd)), sessions, last_exists)) => {
                        (agent, cwd, sessions, last_exists)
                    }
                    Ok((None, ..)) => {
                        // If the chat is gone, the one it took over the
                        // view from still shows when it opens.
                        if let Some(overtaken) = this.lifecycle.give_back(id, request)
                            && this.chats.iter().any(|chat| chat.id == overtaken)
                        {
                            this.activate(overtaken, cx);
                        }
                        return;
                    }
                    Err(err) => {
                        eprintln!("can't open chat {id}: {err}");
                        return;
                    }
                };
                let mut notice = None;
                let resumed = match sessions.last() {
                    Some(last) if last.prompted && last_exists => Some(last),
                    // Claude deletes old transcripts. What the chat kept of the
                    // session stays in its history, and the banner says why a
                    // new session started.
                    Some(last) if last.prompted => {
                        let (session, lost) = (
                            last.id,
                            this.store.lose_transcript(last.id, TRANSCRIPT_GONE),
                        );
                        cx.background_spawn(async move {
                            if let Err(err) = lost.await {
                                eprintln!("can't mark session {session} missing: {err}")
                            }
                        })
                        .detach();
                        notice = Some(TRANSCRIPT_GONE.into());
                        None
                    }
                    // A session without a prompt has nothing to resume.
                    _ => None,
                };
                // A chat the user selected while this one was being read
                // keeps the view. This one still starts, but in the
                // background, so its completion can't steal focus.
                let show = this.lifecycle.is_requested(id);
                // Two CLIs in one session would append to the same transcript,
                // so a session another chat is running can't be resumed here.
                if let Some(last) = resumed
                    && this.live_elsewhere(id, agent, last)
                {
                    this.block_chat(
                        id,
                        agent,
                        &cwd,
                        sessions.last(),
                        "This session is running in another chat. Start a new session to \
                         keep going here."
                            .into(),
                        cx,
                    );
                } else if this.start_chat(id, agent, &cwd, resumed, cx)
                    && let Some(notice) = notice
                    && let Some(chat) = this.chats.iter_mut().find(|chat| chat.id == id)
                {
                    chat.notice = Some(notice);
                }
                if show {
                    this.activate(id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Starts a chat's CLI, resuming `session` if given. The chat stays hidden
    /// until it is activated.
    #[doc(hidden)]
    pub fn start_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        session: Option<&store::Session>,
        cx: &mut Context<Self>,
    ) -> bool {
        let report = uuid::Uuid::new_v4().to_string();
        let colors = self
            .colors
            .expect("the window paints the terminals before any chat starts");
        let mut all_args = self.following.reports.args(agent);
        if let Some(session) = session {
            all_args.extend(agent.resume_args(&session.agent_id, &session.transcript_path));
        }
        let report_path = self.following.reports.path(&report);
        let env = [(reports::ENV, report_path.as_os_str())];
        let terminal = match Pty::spawn(agent.program(), &all_args, &env, cwd, colors) {
            Ok(pty) => {
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
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
                    exited: false,
                    _exited: exited,
                    _status: status,
                })
            }
            Err(err) => {
                let message = format!("Couldn't start {}: {err}", agent.program());
                cx.emit(RuntimeEvent::Notice(message.clone()));
                Err(SharedString::from(message))
            }
        };
        let started = terminal.is_ok();
        self.push_chat(id, agent, cwd, report, terminal, session, cx);
        started
    }

    /// Adds a chat, with its terminal, or with why its CLI didn't start.
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
    ) -> &mut ChatRuntime {
        let (terminal, message) = match terminal {
            Ok(terminal) => (Some(terminal), None),
            Err(message) => (None, Some(message)),
        };
        self.chats.push(ChatRuntime {
            id,
            agent,
            cwd: cwd.to_path_buf(),
            report,
            message,
            session: session.map(|session| session.id),
            transcript: session.map(|session| session.transcript_path.clone()),
            recording: None,
            status: None,
            turn: None,
            unseen: false,
            codex: Codex::default(),
            notice: None,
            fresh_on_restart: false,
            terminal,
        });
        cx.emit(RuntimeEvent::ChatStarted(id));
        self.chats.last_mut().expect("a chat was just pushed")
    }

    /// Whether another chat's CLI is running in a session.
    fn live_elsewhere(&self, chat_id: i64, agent: Agent, session: &store::Session) -> bool {
        self.chats.iter().any(|chat| {
            chat.id != chat_id
                && (chat.current_session() == Some(session.id)
                    // One its CLI reported is in it before it's recorded.
                    || chat.running()
                        && chat.agent == agent
                        && chat.recording.as_ref().is_some_and(|recording| {
                            !recording.done.is_closed() && recording.agent_id == session.agent_id
                        }))
        }) || self.lifecycle.stopped_elsewhere(chat_id, session.id)
    }

    /// Shows a chat whose CLI wasn't started, with the reason above the
    /// history. Restarting starts a new session rather than resuming the one
    /// that couldn't start.
    fn block_chat(
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
    }

    /// Restarts a chat's CLI, starting a new session when the chat couldn't
    /// have resumed its last one.
    pub(crate) fn restart(&mut self, id: i64, cx: &mut Context<Self>) {
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
            if self.start_chat(id, agent, &cwd, None, cx) {
                self.activate(id, cx);
            }
            cx.emit(RuntimeEvent::HistoryChanged {
                sessions: Vec::new(),
            });
        } else {
            self.open_chat(id, cx);
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
            && chat.terminal.as_ref().is_some_and(|terminal| {
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
        if let Some(terminal) = &mut chat.terminal {
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
        cx.emit(RuntimeEvent::ChatExited(id));
        self.tick_turn(cx);
        self.read_last_session(id, session, cx);
    }

    /// Closes a chat whose CLI the user quit, and reads its last session.
    pub(crate) fn close_chat(&mut self, id: i64, cx: &mut Context<Self>) {
        let session = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(|chat| chat.session);
        match self.stop_chat(id, cx) {
            Some(reaped) => self.track_stopping(id, reaped, session, cx),
            None => self.read_last_session(id, session, cx),
        }
    }

    /// Kills the chat's CLI, if it has one, and closes the chat. The channel
    /// closes once the CLI has been reaped.
    #[doc(hidden)]
    pub fn stop_chat(&mut self, id: i64, cx: &mut Context<Self>) -> Option<Reaped> {
        let index = self.chats.iter().position(|chat| chat.id == id)?;
        let chat = self.chats.remove(index);
        let reaped = chat
            .terminal
            .map(|terminal| terminal.terminal.read(cx).shutdown());
        cx.emit(RuntimeEvent::ChatEnded(id));
        if self.active == Some(id) {
            self.active = None;
            cx.emit(RuntimeEvent::ActiveChanged(None));
        }
        self.tick_turn(cx);
        reaped
    }

    /// Finds the chats whose CLI sits at rest with nothing the user would
    /// miss (see `at_rest`), the shown one included, which can be closed and
    /// opened again on the same session.
    pub fn idle_chats(&self, cx: &mut Context<Self>) -> Task<Vec<i64>> {
        let asked: Vec<_> = self
            .chats
            .iter()
            .map(|chat| {
                let last_prompt = chat.session.map(|session| self.store.last_prompt(session));
                let transcript = chat.transcript.clone();
                (
                    chat.id,
                    chat.report.clone(),
                    chat.session,
                    transcript,
                    last_prompt,
                )
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut found = Vec::new();
            for (id, report, session, transcript, last_prompt) in asked {
                let last_prompt = match last_prompt {
                    Some(last_prompt) => last_prompt.await,
                    None => Ok((false, None)),
                };
                let kept =
                    smol::unblock(move || transcript.is_some_and(|path| path.exists())).await;
                found.push((id, report, session, kept, last_prompt));
            }
            this.update(cx, |this, cx| {
                // One that restarted, or went into another session, meanwhile
                // isn't what was read.
                found
                    .into_iter()
                    .filter(|(id, report, session, kept, last_prompt)| {
                        this.chats
                            .iter()
                            .find(|chat| {
                                chat.id == *id && chat.report == *report && chat.session == *session
                            })
                            .is_some_and(|chat| at_rest(chat, *kept, last_prompt, cx))
                    })
                    .map(|(id, ..)| id)
                    .collect()
            })
            .unwrap_or_default()
        })
    }

    /// The session a chat's Codex CLI is in, looked for among the files its
    /// process keeps open, which only that process can tell. `None` for the
    /// other agents, which report their sessions. That's a few reads under
    /// /proc, which a chat stopping now and then can afford on this thread.
    fn codex_session(&self, id: i64, cx: &App) -> Option<(String, PathBuf)> {
        let chat = self.chats.iter().find(|chat| chat.id == id)?;
        let terminal = chat
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
    pub(crate) fn suspend(&mut self, id: i64, cx: &mut Context<Self>) {
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
        self.lifecycle.stop(id, reaped.clone(), session);
        let last = self.last_session(id);
        cx.spawn(async move |this, cx| {
            let session = last.await.or(session);
            this.update(cx, |this, _| {
                this.lifecycle.stopped_in(id, &reaped, session)
            })
            .ok();
            // What the CLI writes as it exits is part of the session.
            reaped.recv().await.ok();
            this.update(cx, |this, cx| {
                this.lifecycle.reaped(id, &reaped);
                this.read_sessions(session.into_iter().collect(), false, cx)
                    .detach();
            })
            .ok();
        })
        .detach();
    }

    /// Shows chat `id`, which takes back what was said about it.
    #[doc(hidden)]
    pub fn activate(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) {
            chat.unseen = false;
        }
        alerts::withdraw(id, cx);
        self.active = Some(id);
        self.lifecycle.shown(id);
        cx.emit(RuntimeEvent::ActiveChanged(Some(id)));
        self.tick_turn(cx);
    }

    fn set_status(&mut self, id: i64, status: Option<Status>, cx: &mut Context<Self>) {
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
        let alert = alert.zip(status).filter(|_| !self.in_view(id));
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
        cx.emit(RuntimeEvent::StatusChanged(id));
        self.tick_turn(cx);
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
        let store = self.store.clone();
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
                if this.in_view(id) {
                    return;
                }
                let mut body = this
                    .describe(id)
                    .unwrap_or_else(|| "A chat in Agents Hub".into());
                if let Some(details) = alerts::details(alert, &items) {
                    body.push('\n');
                    body.push_str(&details);
                }
                let mut summary = activity(agent, Some(Some(status)));
                if let Some(took) = took {
                    summary = format!("{summary} · {}", duration(took));
                }
                alerts::alert(id, hold, alert, summary, body, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Whether the user can see chat `id`: it's shown, and the window says
    /// the user sees it (see `Intent::SetFocused`).
    fn in_view(&self, id: i64) -> bool {
        self.active == Some(id) && self.focused
    }

    /// Takes back the notification about the shown chat once the user can
    /// see it, as when the window takes the focus or the settings close.
    pub(crate) fn set_focused(&mut self, focused: bool, cx: &mut App) {
        self.focused = focused;
        if let Some(id) = self.active
            && self.in_view(id)
        {
            alerts::withdraw(id, cx);
        }
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
                    if this
                        .update(cx, |_, cx| cx.emit(RuntimeEvent::TurnTick))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
    }

    /// Records the session each open Codex chat's CLI is in, but those of
    /// `removed`, from the files its process keeps open, before the process
    /// goes. Gives the sessions recorded.
    #[doc(hidden)]
    pub fn record_codex_sessions(&self, removed: &HashSet<i64>, cx: &App) -> Vec<i64> {
        let mut recorded_sessions = Vec::new();
        for chat in self.chats.iter().filter(|chat| !removed.contains(&chat.id)) {
            if chat.agent == Agent::Codex
                && let Some(terminal) = &chat.terminal
                && let Some((agent_id, path)) =
                    sessions::codex_session(&terminal.terminal.read(cx).pid())
            {
                match smol::block_on(
                    self.store
                        .record_session(chat.id, chat.agent, agent_id, path, None),
                ) {
                    Ok(recorded) => recorded_sessions.push(recorded.session_id),
                    Err(err) => eprintln!("can't record the last Codex session: {err}"),
                }
            }
        }
        recorded_sessions
    }

    /// Records the sessions each open chat's CLI reported last, but those of
    /// `removed`, and reads them along with the session the chat is in.
    #[doc(hidden)]
    pub fn import_chats(&mut self, removed: &HashSet<i64>) {
        for chat in self.chats.iter().filter(|chat| !removed.contains(&chat.id)) {
            if let Err(err) = import_chat(
                &self.store,
                &mut self.following.reports,
                chat.id,
                chat.agent,
                &chat.report,
                chat.session.into_iter().collect(),
            ) {
                eprintln!("can't import chat {} on shutdown: {err}", chat.id);
            }
        }
    }
}
