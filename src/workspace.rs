use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, IconName, StyledExt as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::{RecursiveMode, Watcher as _};

use crate::agent::Agent;
use crate::appearance;
use crate::db::{self, Db};
use crate::history::History;
use crate::reports::{self, Reports};
use crate::sessions;
use crate::sidebar::{Lamp, Sidebar, SidebarEvent, lamp};
use crate::status::Status;
use crate::terminal::{self, Exited, Pty, StatusChanged, Terminal};
use crate::terminal_view::TerminalView;
use crate::transcript;

const SIDEBAR_WIDTH: Pixels = px(280.);

/// The most of the window's height that a chat's earlier sessions take.
const HISTORY_SHARE: f32 = 0.45;

/// How much of the window's height a project's shell takes under its chat.
const SHELL_SHARE: f32 = 0.35;

/// Codex writes its session files in bursts, so they're looked at once it
/// pauses.
const CODEX_DELAY: Duration = Duration::from_millis(500);

/// The most CLIs kept running. Past it, the chats shown longest ago stop once
/// they're at rest, and opening one again resumes its session.
const MAX_RUNNING: usize = 4;

/// Said above a chat whose last session couldn't be resumed, and kept in that
/// session's history.
const TRANSCRIPT_GONE: &str = "This session's transcript is gone, so it couldn't be resumed. \
                               Its saved history is still available here.";

actions!(workspace, [ToggleShell]);

pub fn init(cx: &mut App) {
    // Bound without a context so it reaches the workspace before a focused
    // terminal sends it to its CLI.
    cx.bind_keys([KeyBinding::new("ctrl-`", ToggleShell, None)]);
}

impl SelectItem for Agent {
    type Value = Agent;

    fn title(&self) -> SharedString {
        self.display_name().into()
    }

    fn value(&self) -> &Self::Value {
        self
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .gap_2()
            .child(lamp(appearance::agent_color(*self, cx), Lamp::Off))
            .child(self.display_name())
    }
}

pub struct Workspace {
    sidebar: Entity<Sidebar>,
    db: Db,
    /// Open chats, including exited terminals and history whose CLI couldn't
    /// start, the one shown longest ago first. Running processes keep running
    /// while another chat is shown, up to `MAX_RUNNING`.
    chats: Vec<Chat>,
    /// The chat that is shown.
    active: Option<i64>,
    /// The chat the user last asked to show. Opening a chat reads its last
    /// session before starting its CLI, so a request can finish after a newer
    /// one; only the latest one may take the view.
    requested: Option<i64>,
    /// The chats whose last session is being read before their CLI starts.
    opening: HashSet<i64>,
    /// The chats whose CLI was stopped to free what it held, until it has been
    /// reaped, with the session it was in. Until then the CLI may still write
    /// to that session, so nothing else may run in it.
    stopping: HashMap<i64, (smol::channel::Receiver<()>, Option<i64>)>,
    /// The last Codex session lookup started. Each lookup marks the chats it
    /// looked at with this, so an older lookup that finishes later can be
    /// told apart from a newer one.
    codex_lookup: u64,
    reports: Reports,
    codex_watcher: Option<notify::RecommendedWatcher>,
    /// What the watcher looks at: Codex's session folder, or while it doesn't
    /// exist yet, the closest folder above it.
    codex_watch: Option<(PathBuf, RecursiveMode)>,
    _sidebar_events: Subscription,
    _report_events: Task<()>,
    _codex_events: Task<()>,
    _appearance: Subscription,
    _theme: Subscription,
    new_chat_select: Option<Subscription>,
    /// Why the workspace can't be used: the database didn't open, or another
    /// Agents Hub holds the lock.
    broken: Option<SharedString>,
    /// The sessions whose transcript is being read, which shutdown reads
    /// itself since it can't wait for them. A session is in it once per read
    /// under way, so it stays until the last of them is done.
    reading: Vec<i64>,
    /// The lock that keeps a second instance off the database.
    _lock: Option<fs::File>,
    /// The projects' shells, by folder, for quick commands next to their
    /// chats. Nothing about them is saved, and they run until they exit or
    /// the app quits.
    shells: HashMap<PathBuf, Shell>,
    /// Whether the shown chat's project shell shows under it.
    shell_shown: bool,
    /// Holds the focus when nothing in the workspace does, so its key
    /// bindings still reach it.
    focus_handle: FocusHandle,
}

struct Shell {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    _exited: Subscription,
}

struct Chat {
    id: i64,
    agent: Agent,
    /// Its project's folder, which its CLI runs in.
    cwd: PathBuf,
    /// The name of the file its CLI reports its sessions to.
    report: String,
    terminal: Option<ChatTerminal>,
    /// Startup failure or exit message, shown alongside the saved history.
    message: Option<SharedString>,
    history: Entity<History>,
    /// The session the CLI is in, once known. It changes when the CLI
    /// switches to another session, as on `/clear`.
    session: Option<i64>,
    /// That session's transcript.
    transcript: Option<PathBuf>,
    /// What the CLI last reported doing, if it reports that at all.
    status: Option<Status>,
    /// Whether the CLI finished while another chat was shown, and hasn't been
    /// looked at since.
    unseen: bool,
    /// The Codex session lookup that last looked at this chat. A lookup that
    /// started earlier must not record its result after a newer one, or it
    /// would move the chat back to a session it has left.
    codex_lookup: u64,
    /// Something about the session to show above the terminal, without a
    /// restart button.
    notice: Option<SharedString>,
    /// Whether restarting starts a new session instead of resuming the last.
    fresh_on_restart: bool,
}

struct ChatTerminal {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    exited: bool,
    _exited: Subscription,
    _status: Subscription,
}

impl Chat {
    fn running(&self) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|terminal| !terminal.exited)
    }

    fn current_session(&self) -> Option<i64> {
        self.session.filter(|_| self.running())
    }
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (db, lock, broken) = match db::open_locked() {
            Ok((db, lock)) => (db, lock, None),
            Err(err) => {
                eprintln!("{err:#}");
                (
                    Db::open_in_memory().expect("SQLite opens in memory"),
                    None,
                    Some(format!("{err:#}").into()),
                )
            }
        };

        let sidebar = cx.new(Sidebar::new);
        let sidebar_events =
            cx.subscribe_in(&sidebar, window, |this, _, event, window, cx| match event {
                SidebarEvent::AddProject => this.add_project(window, cx),
                SidebarEvent::RemoveProject(id) => this.confirm_remove_project(*id, window, cx),
                SidebarEvent::NewChat(project_id) => {
                    this.open_new_chat_dialog(*project_id, window, cx)
                }
                SidebarEvent::OpenChat(id) => this.open_chat(*id, window, cx),
                SidebarEvent::RenameChat(id, title) => {
                    this.open_rename_dialog(*id, title.clone(), window, cx)
                }
                SidebarEvent::PinChat(id, pinned) => {
                    if let Err(err) = this.db.set_pinned(*id, *pinned) {
                        eprintln!("can't pin chat {id}: {err}");
                    }
                    this.refresh(cx);
                }
                SidebarEvent::MoveChat(id, to) => {
                    if let Err(err) = this.db.move_chat(*id, *to) {
                        eprintln!("can't move chat {id}: {err}");
                    }
                    this.refresh(cx);
                }
                SidebarEvent::DeleteChat(id) => this.confirm_delete_chat(*id, window, cx),
                SidebarEvent::Export => this.export_database(window, cx),
            });

        let (reports, reported) = Reports::new();
        let report_events = cx.spawn(async move |this, cx| {
            while let Ok(key) = reported.recv().await {
                if this
                    .update(cx, |this, cx| this.read_reports(&key, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let (codex_tx, codex_changed) = smol::channel::unbounded();
        let codex_watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
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
                let found = this.update(cx, |this, cx| {
                    this.update_codex_watch();
                    this.find_codex_sessions(cx);
                });
                if found.is_err() {
                    break;
                }
            }
        });

        // Closing the window releases the workspace before the app quits, and
        // quitting releases it too.
        cx.on_release(|this, cx| {
            // Nothing can be scheduled on a released workspace, so what the
            // CLIs wrote last is read here: the sessions in their queued
            // reports, the ones they were in, and the reads still under way.
            // The sessions they left before were read when they left them.
            let mut sessions = this.reading.clone();
            sessions.extend(this.stopping.values().filter_map(|(_, session)| *session));
            // A chat being opened reads its last session on a thread of its
            // own, which isn't in `reading`, so it is read again here, the
            // way `open_chat` reads it.
            for &id in &this.opening {
                if let Some(last) = this.sessions_of(id).pop()
                    && last.transcript_path.exists()
                {
                    sessions.push(last.id);
                }
            }
            // Discover Codex's last session before its process disappears.
            // A queued watcher callback cannot run once we're released.
            for chat in &this.chats {
                if chat.agent == Agent::Codex
                    && let Some(terminal) = &chat.terminal
                    && let Some(path) = terminal
                        .terminal
                        .read(cx)
                        .with_pid(sessions::codex_session_of)
                    && let Some(agent_id) = transcript::session_id(chat.agent, &path)
                {
                    match this
                        .db
                        .record_session(chat.id, chat.agent, &agent_id, &path, None)
                    {
                        Ok(recorded) => {
                            sessions.extend(recorded.left.into_iter().chain([recorded.session_id]))
                        }
                        Err(err) => eprintln!("can't record the last Codex session: {err}"),
                    }
                }
            }
            // The CLIs are killed on threads of their own, which the app would
            // otherwise exit before they are done with.
            let reaped: Vec<_> = this
                .chats
                .iter()
                .filter_map(|chat| chat.terminal.as_ref())
                .map(|terminal| terminal.terminal.read(cx).shutdown())
                .chain(this.stopping.values().map(|(reaped, _)| reaped.clone()))
                .chain(
                    this.shells
                        .values()
                        .map(|shell| shell.terminal.read(cx).shutdown()),
                )
                .collect();
            terminal::wait_reaped(reaped);
            for chat in &this.chats {
                if let Err(err) = import_chat(
                    &this.db,
                    &mut this.reports,
                    chat.id,
                    chat.agent,
                    &chat.report,
                    chat.session.into_iter().collect(),
                ) {
                    eprintln!("can't import chat {} on shutdown: {err}", chat.id);
                }
            }
            sessions.sort_unstable();
            sessions.dedup();
            for id in sessions {
                if let Err(err) = transcript::read(&this.db, id) {
                    eprintln!("can't read session {id} on shutdown: {err}");
                }
            }
            this.reports.remove();
        })
        .detach();

        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx)
        });
        let theme = cx.observe_global::<Theme>(|this, cx| {
            let colors = appearance::terminal_colors(cx);
            for chat in &this.chats {
                if let Some(terminal) = &chat.terminal {
                    terminal.terminal.read(cx).set_colors(colors);
                }
            }
            for shell in this.shells.values() {
                shell.terminal.read(cx).set_colors(colors);
            }
        });

        let mut this = Self {
            sidebar,
            db,
            chats: Vec::new(),
            active: None,
            requested: None,
            opening: HashSet::new(),
            stopping: HashMap::new(),
            codex_lookup: 0,
            reports,
            codex_watcher,
            codex_watch: None,
            _sidebar_events: sidebar_events,
            _report_events: report_events,
            _codex_events: codex_events,
            _appearance: appearance,
            _theme: theme,
            new_chat_select: None,
            broken,
            reading: Vec::new(),
            _lock: lock,
            shells: HashMap::new(),
            shell_shown: false,
            focus_handle: cx.focus_handle(),
        };
        this.update_codex_watch();
        this.refresh(cx);
        this
    }

    fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a folder: {err}"), cx)
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let path = fs::canonicalize(&path).unwrap_or(path);
                match this.db.add_project(&path) {
                    Ok(id) => {
                        this.refresh(cx);
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.expand(id, cx));
                    }
                    Err(err) => window
                        .push_notification(format!("Couldn't add {}: {err}", path.display()), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Copies the database where the user asks, so the history can live
    /// outside the one file agentshub reads.
    fn export_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let name = format!("agentshub-{}.db", jiff::Zoned::now().date());
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a file: {err}"), cx)
                    })
                    .ok();
                    return;
                }
                _ => return,
            };
            this.update_in(cx, |this, window, cx| match this.db.export(&path) {
                Ok(()) => window
                    .push_notification(format!("Exported the database to {}", path.display()), cx),
                Err(err) => {
                    window.push_notification(format!("Couldn't export the database: {err}"), cx)
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_remove_project(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Remove this project?")
                .description(
                    "Its agents are stopped, and its chats are deleted from Agents Hub. \
                     The folder and the agents' own session files stay.",
                )
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    this.update(cx, |this, cx| {
                        let projects = this.db.projects().unwrap_or_default();
                        let project = projects.iter().find(|project| project.id == id);
                        for chat in project.into_iter().flat_map(|project| &project.chats) {
                            this.stop_chat(chat.id, cx);
                        }
                        // Dropping its terminal kills the shell.
                        if let Some(project) = project {
                            this.shells.remove(&project.path);
                            this.sync_shells(cx);
                        }
                        if let Err(err) = this.db.remove_project(id) {
                            eprintln!("can't remove project {id}: {err}");
                        }
                        this.refresh(cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn confirm_delete_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Delete this chat?")
                .description(
                    "Its agent is stopped, and the chat is deleted from Agents Hub. \
                     The agent's own session files stay.",
                )
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    this.update(cx, |this, cx| {
                        this.stop_chat(id, cx);
                        if let Err(err) = this.db.delete_chat(id) {
                            eprintln!("can't delete chat {id}: {err}");
                        }
                        this.refresh(cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn open_rename_dialog(
        &mut self,
        id: i64,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(title.clone())
                .placeholder("Its agent's title")
        });
        let this = cx.entity().downgrade();
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                input.focus_handle(cx).focus(window, cx);
                input.update(cx, |input, cx| input.select_all(window, cx));
            }
            let (this, input, title) = (this.clone(), input.clone(), title.clone());
            dialog
                .title("Rename this chat")
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Leave it empty to title it after its agent's sessions."),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("rename").primary().label("Rename")),
                        ),
                )
                // Enter in the input confirms too.
                .on_ok(move |_, _, cx| {
                    let value = input.read(cx).value();
                    // An untouched title stays the agent's, and follows it.
                    if value != title {
                        this.update(cx, |this, cx| {
                            if let Err(err) = this.db.rename_chat(id, &value) {
                                eprintln!("can't rename chat {id}: {err}");
                            }
                            this.refresh(cx);
                        })
                        .ok();
                    }
                    true
                })
        });
    }

    fn open_new_chat_dialog(
        &mut self,
        project_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = match self.db.projects() {
            Ok(projects) => projects
                .into_iter()
                .find(|project| project.id == project_id),
            Err(err) => {
                eprintln!("can't list the projects: {err}");
                None
            }
        };
        let Some(project) = project else {
            return;
        };
        let select = cx.new(|cx| SelectState::new(Agent::ALL.to_vec(), None, window, cx));
        let project_label = SharedString::from(project.path.to_string_lossy().into_owned());
        self.new_chat_select =
            Some(
                cx.subscribe_in(&select, window, move |this, _, event, window, cx| {
                    if let SelectEvent::Confirm(Some(agent)) = event {
                        window.close_dialog(cx);
                        this.new_chat(project_id, *agent, &project.path, window, cx);
                    }
                }),
            );

        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                select.focus_handle(cx).focus(window, cx);
            }
            dialog.title("Start a coding agent").child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(project_label.clone()),
                    )
                    .child(Select::new(&select).placeholder("Choose an agent")),
            )
        });
    }

    fn new_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        cwd: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = match self.db.add_chat(project_id, agent) {
            Ok(id) => id,
            Err(err) => {
                window.push_notification(format!("Couldn't add the chat: {err}"), cx);
                return;
            }
        };
        // Its first session report adds its first session.
        if self.start_chat(id, agent, cwd, None, window, cx) {
            self.activate(id, window, cx);
        } else {
            self.stop_chat(id, cx);
            self.db.delete_chat(id).ok();
        }
        self.refresh(cx);
    }

    /// Shows a chat, resuming its last session if its CLI doesn't run.
    fn open_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.chats.iter().any(|chat| chat.id == id) {
            // Reading a chat's last session takes long enough for another
            // chat to be asked for meanwhile. This is the request that a
            // completion compares itself with.
            self.requested = Some(id);
            self.activate(id, window, cx);
            return;
        }
        let (agent, cwd) = match self.db.chat(id) {
            Ok(Some(chat)) => chat,
            Ok(None) => return,
            Err(err) => {
                eprintln!("can't open chat {id}: {err}");
                return;
            }
        };
        // A CLI that was stopped to free what it held opens again once it's
        // gone, so the two never run in one session.
        if let Some((reaped, _)) = self
            .stopping
            .get(&id)
            .filter(|(reaped, _)| !reaped.is_closed())
        {
            let reaped = reaped.clone();
            self.requested = Some(id);
            cx.spawn_in(window, async move |this, cx| {
                reaped.recv().await.ok();
                this.update_in(cx, |this, window, cx| {
                    if this.requested == Some(id) {
                        this.open_chat(id, window, cx);
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        // The chat exists, so this request is now the one that decides what
        // the view shows when the read below finishes.
        self.requested = Some(id);
        // Clicks that arrive while the session is read would start a second
        // CLI in the same chat.
        if !self.opening.insert(id) {
            return;
        }

        // The last session decides how the CLI starts, so it's read first: the
        // CLI may have gone on without agentshub. Transcripts can be large
        // enough that this can't happen while the window waits.
        let db = self.db.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = smol::unblock(move || -> anyhow::Result<()> {
                if let Some(last) = db.sessions(id)?.last()
                    && last.transcript_path.exists()
                {
                    transcript::read(&db, last.id)?;
                    // Opening a chat is the chat being active, which orders the
                    // sidebar. Reading its history must not do the same.
                    db.touch_session(last.id)?;
                }
                Ok(())
            })
            .await;
            if let Err(err) = read {
                eprintln!("can't read the last session of chat {id}: {err}");
            }
            this.update_in(cx, |this, window, cx| {
                this.opening.remove(&id);
                // The chat may have been deleted, or opened elsewhere, meanwhile.
                if this.chats.iter().any(|chat| chat.id == id) {
                    return;
                }
                match this.db.chat(id) {
                    Ok(Some(_)) => {}
                    Ok(None) => return,
                    Err(err) => {
                        eprintln!("can't open chat {id}: {err}");
                        return;
                    }
                }
                let sessions = this.sessions_of(id);
                let mut notice = None;
                let resumed = match sessions.last() {
                    Some(last) if last.prompted && last.transcript_path.exists() => Some(last),
                    // Claude deletes old transcripts. What the chat kept of the
                    // session stays in its history, and the banner says why a
                    // new session started.
                    Some(last) if last.prompted => {
                        match this.db.set_missing(last.id) {
                            Ok(true) => {
                                this.db.add_notice(last.id, TRANSCRIPT_GONE).ok();
                            }
                            Ok(false) => {}
                            Err(err) => eprintln!("can't mark session {} missing: {err}", last.id),
                        }
                        notice = Some(TRANSCRIPT_GONE.into());
                        None
                    }
                    // A session without a prompt has nothing to resume.
                    _ => None,
                };
                // A chat the user selected while this one was being read
                // keeps the view. This one still starts, but in the
                // background, so its completion can't steal focus.
                let show = this.requested == Some(id);
                // Two CLIs in one session would append to the same transcript,
                // so a session another chat is running can't be resumed here.
                if let Some(last) = resumed
                    && this.live_elsewhere(id, last.id)
                {
                    this.block_chat(
                        id,
                        agent,
                        &cwd,
                        "This session is running in another chat. Start a new session to \
                         keep going here."
                            .into(),
                        cx,
                    );
                } else if this.start_chat(id, agent, &cwd, resumed, window, cx)
                    && let Some(notice) = notice
                    && let Some(chat) = this.chats.iter_mut().find(|chat| chat.id == id)
                {
                    chat.notice = Some(notice);
                }
                if show {
                    this.activate(id, window, cx);
                } else {
                    this.sync_sidebar(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn sessions_of(&self, chat_id: i64) -> Vec<db::Session> {
        self.db.sessions(chat_id).unwrap_or_else(|err| {
            eprintln!("can't list the sessions of chat {chat_id}: {err}");
            Vec::new()
        })
    }

    fn read_reports(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some((id, agent)) = self
            .chats
            .iter()
            .find(|chat| chat.report == key)
            .map(|chat| (chat.id, chat.agent))
        else {
            return;
        };
        let mut recorded = false;
        for (agent_id, report) in self.reports.read(key, agent) {
            recorded |= self.record_session(
                id,
                &agent_id,
                &report.transcript_path,
                report.reason.as_deref(),
                cx,
            );
        }
        // Reports arrive in bursts, and each one used to reload the sidebar.
        if recorded {
            self.refresh(cx);
        }
    }

    /// Codex writes its session files as it goes, so the sessions of its
    /// terminals are looked for whenever those change.
    fn find_codex_sessions(&mut self, cx: &mut Context<Self>) {
        // Each chat's report name identifies this terminal, not just its row:
        // A chat can restart its terminal while the lookup runs.
        //
        // A lookup can take longer than the watcher's delay, so a newer one
        // can finish first. Marking the chats a lookup looks at with its
        // generation lets a result that has been superseded be dropped rather
        // than move a chat back to the session it has already left.
        let lookup = self.codex_lookup.wrapping_add(1);
        let mut chats = Vec::new();
        for chat in &mut self.chats {
            if chat.agent != Agent::Codex {
                continue;
            }
            let Some(terminal) = &chat.terminal else {
                continue;
            };
            let Some(pid) = terminal.terminal.read(cx).with_pid(Some) else {
                continue;
            };
            chat.codex_lookup = lookup;
            chats.push((chat.id, chat.report.clone(), pid));
        }
        if chats.is_empty() {
            return;
        }
        self.codex_lookup = lookup;
        cx.spawn(async move |this, cx| {
            // Looking through a process's open files, and reading the first
            // line of the newest one, doesn't belong on the window's thread.
            let found = smol::unblock(move || {
                chats
                    .into_iter()
                    .filter_map(|(id, report, pid)| {
                        let path = sessions::codex_session_of(pid)?;
                        let agent_id = transcript::session_id(Agent::Codex, &path)?;
                        Some((id, report, path, agent_id))
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            this.update(cx, |this, cx| {
                let mut recorded = false;
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
                            chat.codex_lookup == lookup && chat.transcript.as_ref() != Some(&path)
                        });
                    if !current {
                        continue;
                    }
                    recorded |= this.record_session(id, &agent_id, &path, None, cx);
                }
                if recorded {
                    this.refresh(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn update_codex_watch(&mut self) {
        let (Some(watcher), Some(dir)) = (&mut self.codex_watcher, sessions::codex_sessions_dir())
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
        if wanted == self.codex_watch {
            return;
        }
        if let Some((path, _)) = self.codex_watch.take() {
            watcher.unwatch(&path).ok();
        }
        if let Some((path, mode)) = wanted {
            match watcher.watch(&path, mode) {
                Ok(()) => self.codex_watch = Some((path, mode)),
                Err(err) => eprintln!("can't watch {}: {err}", path.display()),
            }
        }
    }

    /// Records the session a chat's CLI went into, and reads the one it left.
    /// Says whether it recorded anything.
    fn record_session(
        &mut self,
        chat_id: i64,
        agent_id: &str,
        path: &Path,
        reason: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.chats.iter().position(|chat| chat.id == chat_id) else {
            return false;
        };
        let (id, agent) = (self.chats[index].id, self.chats[index].agent);
        self.chats[index].transcript = Some(path.to_path_buf());
        match self.db.record_session(id, agent, agent_id, path, reason) {
            Ok(recorded) => {
                self.chats[index].session = Some(recorded.session_id);
                if let Some(left) = recorded.left {
                    self.read_sessions(vec![left], false, cx).detach();
                }
                true
            }
            Err(err) => {
                eprintln!("can't record session {agent_id}: {err}");
                false
            }
        }
    }

    /// Reads sessions' transcripts on a blocking thread, then shows what
    /// changed. Marks their chats active when `touch` is set, as a finished
    /// turn does but browsing history doesn't.
    fn read_sessions(
        &mut self,
        sessions: Vec<i64>,
        touch: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let db = self.db.clone();
        self.reading.extend(&sessions);
        cx.spawn(async move |this, cx| {
            if sessions.is_empty() {
                return;
            }
            // Parsing transcripts and writing them to the database blocks, so
            // it stays off the executor's threads.
            let (sessions, changed) = smol::unblock(move || {
                let mut changed = Vec::new();
                for &id in &sessions {
                    match transcript::read(&db, id) {
                        Ok(read) => {
                            if touch {
                                db.touch_session(id).ok();
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
                (sessions, changed)
            })
            .await;
            this.update(cx, |this, cx| {
                for id in &sessions {
                    if let Some(index) = this.reading.iter().position(|reading| reading == id) {
                        this.reading.swap_remove(index);
                    }
                }
                this.refresh_after(&changed, cx);
                // A prompt read just now can show a chat's draft was sent.
                this.evict(cx);
            })
            .ok();
        })
    }

    /// Lists the projects and chats again, and the earlier sessions of the
    /// chats whose CLI runs.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_after(&[], cx);
    }

    /// The same, reloading the expanded sessions whose transcript read, in
    /// `changed`, found something.
    fn refresh_after(&mut self, changed: &[i64], cx: &mut Context<Self>) {
        match self.db.projects() {
            Ok(projects) => self
                .sidebar
                .update(cx, |sidebar, cx| sidebar.set_projects(projects, cx)),
            Err(err) => eprintln!("can't list the projects: {err}"),
        }
        for chat in &self.chats {
            let current = chat.current_session();
            chat.history
                .update(cx, |history, cx| history.refresh(current, changed, cx));
        }
    }

    fn set_status(&mut self, id: i64, status: Option<Status>, cx: &mut Context<Self>) {
        let shown = self.active == Some(id);
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == id) else {
            return;
        };
        let busy = |status| matches!(status, Some(Status::Working | Status::Blocked(_)));
        let was_busy = busy(chat.status);
        chat.unseen = match status {
            Some(Status::Done | Status::Error) => !shown,
            // Codex goes back to idle without saying it's done. The others
            // say so, and go back to idle when a turn is cancelled.
            Some(Status::Idle) => chat.unseen || (chat.agent == Agent::Codex && was_busy && !shown),
            _ => false,
        };
        chat.status = status;
        // The session is read after each turn, so the chat's title follows it.
        if was_busy
            && !busy(status)
            && let Some(session) = chat.session
        {
            self.read_sessions(vec![session], true, cx).detach();
        }
        self.sync_sidebar(cx);
        self.evict(cx);
    }

    /// Marks the chat that is shown and what each running one is doing.
    fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
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
    }

    /// Starts a chat's CLI, resuming `session` if given. The chat stays hidden
    /// until it is activated.
    fn start_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        session: Option<&db::Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let report = uuid::Uuid::new_v4().to_string();
        let colors = appearance::terminal_colors(cx);
        let mut all_args = self.reports.args(agent);
        if let Some(session) = session {
            all_args.extend(agent.resume_args(&session.agent_id, &session.transcript_path));
        }
        let report_path = self.reports.path(&report);
        let env = [(reports::ENV, report_path.as_os_str())];
        let terminal = match Pty::spawn(agent.program(), &all_args, &env, cwd, colors) {
            Ok(pty) => {
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
                let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
                // Hidden terminals don't need to build frames.
                terminal.read(cx).set_visible(false);
                let exited = cx.subscribe(&terminal, move |this, _, _: &Exited, cx| {
                    this.chat_exited(id, cx)
                });
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
    #[allow(clippy::too_many_arguments)]
    fn push_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        report: String,
        terminal: Result<ChatTerminal, SharedString>,
        session: Option<&db::Session>,
        cx: &mut Context<Self>,
    ) -> &mut Chat {
        let (terminal, message) = match terminal {
            Ok(terminal) => (Some(terminal), None),
            Err(message) => (None, Some(message)),
        };
        // Without a running CLI, even its last session belongs in history.
        let current = terminal.as_ref().and(session.map(|session| session.id));
        let transcript = session.map(|session| session.transcript_path.clone());
        let db = self.db.clone();
        let history = cx.new(|cx| {
            let mut history = History::new(db, id);
            history.refresh(current, &[], cx);
            history
        });
        self.chats.push(Chat {
            id,
            agent,
            cwd: cwd.to_path_buf(),
            report,
            terminal,
            message,
            history,
            session: session.map(|session| session.id),
            transcript,
            status: None,
            unseen: false,
            codex_lookup: 0,
            notice: None,
            fresh_on_restart: false,
        });
        self.chats.last_mut().expect("a chat was just pushed")
    }

    /// Whether another chat's CLI is running in a session.
    fn live_elsewhere(&self, chat_id: i64, session_id: i64) -> bool {
        self.chats
            .iter()
            .any(|chat| chat.id != chat_id && chat.current_session() == Some(session_id))
            || self.stopping.iter().any(|(&id, (reaped, session))| {
                id != chat_id && *session == Some(session_id) && !reaped.is_closed()
            })
    }

    /// Shows a chat whose CLI wasn't started, with the reason above the
    /// history. Restarting starts a new session rather than resuming the one
    /// that couldn't start.
    fn block_chat(
        &mut self,
        id: i64,
        agent: Agent,
        cwd: &Path,
        message: SharedString,
        cx: &mut Context<Self>,
    ) {
        let session = self
            .db
            .sessions(id)
            .ok()
            .and_then(|mut sessions| sessions.pop());
        let report = uuid::Uuid::new_v4().to_string();
        self.push_chat(id, agent, cwd, report, Err(message), session.as_ref(), cx)
            .fresh_on_restart = true;
        self.sync_sidebar(cx);
        cx.notify();
    }

    /// Restarts a chat's CLI, starting a new session when the chat couldn't
    /// have resumed its last one.
    fn restart(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = self
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .is_some_and(|chat| chat.fresh_on_restart);
        self.stop_chat(id, cx);
        if fresh {
            let Ok(Some((agent, cwd))) = self.db.chat(id) else {
                return;
            };
            if self.start_chat(id, agent, &cwd, None, window, cx) {
                self.activate(id, window, cx);
            }
            self.refresh(cx);
        } else {
            self.open_chat(id, window, cx);
        }
    }

    /// Archives the last session, retaining the terminal's exit diagnostics
    /// until the user restarts or deletes the chat.
    fn chat_exited(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(index) = self.chats.iter().position(|chat| chat.id == id) else {
            return;
        };
        // A report can still be queued when the CLI's exit is handled. It
        // says which session the final read follows, so it is read before the
        // last session is imported.
        let report = self.chats[index].report.clone();
        self.read_reports(&report, cx);
        let chat = &mut self.chats[index];
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
        chat.history
            .update(cx, |history, cx| history.refresh(None, &[], cx));
        self.sync_sidebar(cx);
        cx.notify();
        self.read_sessions(session.into_iter().collect(), false, cx)
            .detach();
    }

    /// Kills the chat's CLI, if it has one, and closes the chat. The channel
    /// closes once the CLI has been reaped.
    fn stop_chat(
        &mut self,
        id: i64,
        cx: &mut Context<Self>,
    ) -> Option<smol::channel::Receiver<()>> {
        let index = self.chats.iter().position(|chat| chat.id == id)?;
        let chat = self.chats.remove(index);
        let reaped = chat
            .terminal
            .map(|terminal| terminal.terminal.read(cx).shutdown());
        if self.active == Some(id) {
            self.active = None;
        }
        self.sync_shells(cx);
        self.sync_sidebar(cx);
        cx.notify();
        reaped
    }

    /// Whether a chat's CLI can be stopped without losing anything the user
    /// would miss: it's at rest, not shown, has nothing unseen, holds no draft,
    /// and can resume its session. One that never said what it's doing keeps
    /// running.
    fn idle(&self, chat: &Chat, cx: &App) -> bool {
        let last_prompt = || match chat.session {
            Some(session) => self.db.last_prompt(session),
            None => Ok((false, None)),
        };
        self.active != Some(chat.id)
            && !chat.unseen
            && matches!(chat.status, Some(Status::Idle | Status::Done))
            && chat.terminal.as_ref().is_some_and(|terminal| {
                !terminal.exited
                    && terminal
                        .terminal
                        .read(cx)
                        .input()
                        .sent(|| last_prompt().ok().and_then(|(_, at)| at))
            })
            // Without its transcript, a session with a prompt can't be resumed,
            // and the CLI holds the only copy of it. Claude deletes transcripts
            // that went untouched for 30 days.
            && (chat.transcript.as_ref().is_some_and(|path| path.exists())
                || matches!(last_prompt(), Ok((false, _))))
    }

    /// Stops the CLIs of the idle chats shown longest ago while more than
    /// `MAX_RUNNING` run.
    fn evict(&mut self, cx: &mut Context<Self>) {
        let running = self.chats.iter().filter(|chat| chat.running()).count();
        let evicted: Vec<_> = self
            .chats
            .iter()
            .filter(|chat| self.idle(chat, cx))
            .take(running.saturating_sub(MAX_RUNNING))
            .map(|chat| chat.id)
            .collect();
        for id in evicted {
            self.suspend(id, cx);
        }
    }

    /// Stops a chat's CLI after recording the session it's in, which opening
    /// the chat again resumes, and reads that session once the CLI is gone.
    fn suspend(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.iter().find(|chat| chat.id == id) else {
            return;
        };
        let report = chat.report.clone();
        // Codex doesn't report its sessions, so the one it's in is looked for
        // among its open files before it goes. That's a few reads under /proc,
        // which a chat stopping now and then can afford on this thread.
        let codex = chat
            .terminal
            .as_ref()
            .filter(|_| chat.agent == Agent::Codex)
            .and_then(|terminal| {
                terminal
                    .terminal
                    .read(cx)
                    .with_pid(sessions::codex_session_of)
            })
            .and_then(|path| Some((transcript::session_id(Agent::Codex, &path)?, path)));
        // A queued report can say the CLI went into another session.
        self.read_reports(&report, cx);
        if let Some((agent_id, path)) = codex {
            self.record_session(id, &agent_id, &path, None, cx);
        }
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
        self.stopping.insert(id, (reaped.clone(), session));
        cx.spawn(async move |this, cx| {
            // What the CLI writes as it exits is part of the session.
            reaped.recv().await.ok();
            this.update(cx, |this, cx| {
                // The chat may have been opened and stopped again since.
                if this
                    .stopping
                    .get(&id)
                    .is_some_and(|(stopping, _)| stopping.same_channel(&reaped))
                {
                    this.stopping.remove(&id);
                }
                this.read_sessions(session.into_iter().collect(), false, cx)
                    .detach();
            })
            .ok();
        })
        .detach();
    }

    fn activate(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        // The chats stay in the order they were last shown in, which is the
        // order idle ones are stopped in.
        if let Some(index) = self.chats.iter().position(|chat| chat.id == id) {
            let chat = self.chats.remove(index);
            self.chats.push(chat);
        }
        for chat in &mut self.chats {
            let shown = chat.id == id;
            if let Some(terminal) = &chat.terminal {
                terminal.terminal.read(cx).set_visible(shown);
            }
            if shown {
                chat.unseen = false;
            }
        }
        self.active = Some(id);
        self.requested = Some(id);
        self.focus_chat(window, cx);
        self.sync_shells(cx);
        self.sync_sidebar(cx);
        self.evict(cx);
        cx.notify();
    }

    fn active_cwd(&self) -> Option<&Path> {
        self.chats
            .iter()
            .find(|chat| Some(chat.id) == self.active)
            .map(|chat| chat.cwd.as_path())
    }

    fn shown_shell(&self) -> Option<&Shell> {
        self.shells
            .get(self.active_cwd()?)
            .filter(|_| self.shell_shown)
    }

    /// Only the shown shell builds frames.
    fn sync_shells(&self, cx: &App) {
        let shown = self.active_cwd().filter(|_| self.shell_shown);
        for (cwd, shell) in &self.shells {
            shell
                .terminal
                .read(cx)
                .set_visible(Some(cwd.as_path()) == shown);
        }
    }

    /// Focuses the shown chat's CLI, or the workspace if it has none running.
    fn focus_chat(&self, window: &mut Window, cx: &mut App) {
        match self
            .chats
            .iter()
            .find(|chat| Some(chat.id) == self.active)
            .and_then(|chat| chat.terminal.as_ref())
        {
            Some(terminal) => terminal.view.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// Hides the shell when it has the focus. Otherwise shows and focuses the
    /// shown chat's project shell, starting it if it isn't running.
    fn toggle_shell(&mut self, _: &ToggleShell, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.active_cwd().map(Path::to_path_buf) else {
            return;
        };
        if let Some(shell) = self.shown_shell()
            && shell.view.focus_handle(cx).is_focused(window)
        {
            self.shell_shown = false;
            self.focus_chat(window, cx);
        } else {
            if !self.shells.contains_key(&cwd) {
                let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let colors = appearance::terminal_colors(cx);
                let pty = match Pty::spawn(&program, &[], &[], &cwd, colors) {
                    Ok(pty) => pty,
                    Err(err) => {
                        window.push_notification(format!("Couldn't start {program}: {err}"), cx);
                        return;
                    }
                };
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
                let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
                let exited_cwd = cwd.clone();
                let exited =
                    cx.subscribe_in(&terminal, window, move |this, _, _: &Exited, window, cx| {
                        this.shell_exited(&exited_cwd, window, cx)
                    });
                self.shells.insert(
                    cwd.clone(),
                    Shell {
                        terminal,
                        view,
                        _exited: exited,
                    },
                );
            }
            self.shell_shown = true;
            self.shells[&cwd].view.focus_handle(cx).focus(window, cx);
        }
        self.sync_shells(cx);
        cx.notify();
    }

    /// Closes a shell that exited, hiding the panel if it showed it.
    fn shell_exited(&mut self, cwd: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_cwd() == Some(cwd) && self.shell_shown {
            self.shell_shown = false;
            if self
                .shells
                .get(cwd)
                .is_some_and(|shell| shell.view.focus_handle(cx).is_focused(window))
            {
                self.focus_chat(window, cx);
            }
        }
        self.shells.remove(cwd);
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if let Some(reason) = self.broken.clone() {
            return render_broken(reason, cx);
        }
        let active = self
            .active
            .and_then(|id| self.chats.iter().find(|chat| chat.id == id));
        let history_max = window.viewport_size().height * HISTORY_SHARE;
        let shell_height = window.viewport_size().height * SHELL_SHARE;
        let shell = self.shown_shell().map(|shell| shell.view.clone());

        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_shell))
            // Terminal output re-renders the window many times a second; the
            // sidebar and the history only need to render again when their own
            // state changes.
            .child(
                self.sidebar.clone().cached(
                    StyleRefinement::default()
                        .w(SIDEBAR_WIDTH)
                        .h_full()
                        .flex_none(),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .map(|this| match active {
                        Some(chat) => {
                            let id = chat.id;
                            let history_max = if chat.terminal.is_none() {
                                window.viewport_size().height
                                    - px(80.)
                                    - if shell.is_some() {
                                        shell_height
                                    } else {
                                        px(0.)
                                    }
                            } else {
                                history_max
                            };
                            this.when_some(chat.notice.clone(), |this, notice| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px_3()
                                        .py_2()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(notice),
                                )
                            })
                            .when_some(chat.message.clone(), |this, message| {
                                this.child(
                                    h_flex()
                                        .p_3()
                                        .gap_3()
                                        .child(div().flex_1().text_sm().child(message))
                                        .child(
                                            Button::new("restart-agent")
                                                .label("Restart agent")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.restart(id, window, cx);
                                                    },
                                                )),
                                        ),
                                )
                            })
                            .children(chat.history.read(cx).height(history_max).map(|height| {
                                chat.history.clone().cached(
                                    StyleRefinement::default().w_full().h(height).flex_none(),
                                )
                            }))
                            .children(chat.terminal.as_ref().map(|terminal| {
                                div().flex_1().min_h_0().child(terminal.view.clone())
                            }))
                            .when_some(shell, |this, shell| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .h(shell_height)
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(shell),
                                )
                            })
                        }
                        None => this.child(render_empty(cx)),
                    }),
            )
            .into_any_element()
    }
}

/// Records the sessions a chat's CLI reported last, and reads `sessions`
/// along with the ones those reports moved the chat into or out of. One
/// transcript that can't be read doesn't stop the others.
fn import_chat(
    db: &Db,
    reports: &mut Reports,
    id: i64,
    agent: Agent,
    key: &str,
    mut sessions: Vec<i64>,
) -> anyhow::Result<()> {
    let mut failure = None;
    for (agent_id, report) in reports.read(key, agent) {
        let path = &report.transcript_path;
        match db.record_session(id, agent, &agent_id, path, report.reason.as_deref()) {
            Ok(recorded) => sessions.extend(recorded.left.into_iter().chain([recorded.session_id])),
            Err(err) => {
                failure.get_or_insert(anyhow::Error::from(err));
            }
        }
    }
    sessions.sort_unstable();
    sessions.dedup();
    for session in sessions {
        if let Err(err) = transcript::read(db, session) {
            failure.get_or_insert(err);
        }
    }
    match failure {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// What shows when the database or the instance lock stopped the workspace
/// from opening. Nothing else is reachable, since nothing could be saved.
fn render_broken(reason: SharedString, cx: &Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_8()
        .bg(theme.background)
        .text_color(theme.foreground)
        .child(
            v_flex()
                .max_w(px(440.))
                .gap_4()
                .child(
                    div()
                        .text_xl()
                        .font_semibold()
                        .child("Agents Hub can't start"),
                )
                .child(
                    div()
                        .text_sm()
                        .line_height(rems(1.4))
                        .text_color(theme.muted_foreground)
                        .child(reason),
                )
                .child(
                    Button::new("quit")
                        .label("Quit")
                        .on_click(|_, _, cx| cx.quit()),
                ),
        )
        .into_any_element()
}

/// What shows while no chat is open: how to start one, and how to read the
/// sidebar's lamps.
fn render_empty(cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let legend_item =
        |color, state, label: &'static str| h_flex().gap_2().child(lamp(color, state)).child(label);

    h_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            v_flex()
                .max_w(px(440.))
                .gap_6()
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xl()
                                .font_semibold()
                                .child("Add a project, then start a chat in it"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .line_height(rems(1.4))
                                .text_color(theme.muted_foreground)
                                .child(
                                    "A project is a folder you work in. Once it's added, \
                                     press + beside it to start Claude Code, Codex or Pi \
                                     there. Choose a chat on the left to reopen it where it \
                                     left off. Agents keep running while you look at \
                                     another chat.",
                                ),
                        )
                        .child(
                            h_flex().pt_2().child(
                                Button::new("add-first-project")
                                    .icon(IconName::FolderOpen)
                                    .label("Add project")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_project(window, cx)
                                    })),
                            ),
                        ),
                )
                .child(
                    v_flex()
                        .pt_5()
                        .gap_3()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_sm()
                        .child(h_flex().gap_5().children(Agent::ALL.map(|agent| {
                            legend_item(
                                appearance::agent_color(agent, cx),
                                Lamp::Off,
                                agent.display_name(),
                            )
                        })))
                        .child(
                            h_flex()
                                .gap_5()
                                .text_color(theme.muted_foreground)
                                .child(legend_item(theme.muted_foreground, Lamp::Off, "Saved"))
                                .child(legend_item(theme.muted_foreground, Lamp::Dim, "Open"))
                                .child(legend_item(theme.muted_foreground, Lamp::Lit, "Working")),
                        ),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::import_chat;
    use crate::{agent::Agent, db::Db, reports::Reports, transcript};
    use serde_json::json;
    use std::fs;

    fn message(id: &str, parent: Option<&str>, role: &str, text: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type": role, "uuid": id, "parentUuid": parent,
                "message": { "role": role, "content": [{ "type": "text", "text": text }] }
            })
        )
    }

    #[test]
    fn shutdown_imports_unread_turns_and_sessions_in_queued_reports() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agentshub.db");
        let db = Db::open(&path).unwrap();
        let project = db.add_project(&dir).unwrap();
        let chat = db.add_chat(project, Agent::Claude).unwrap();
        let first = dir.join("first.jsonl");
        let second = dir.join("second.jsonl");
        let prompt = message("1", None, "user", "first prompt");
        fs::write(&first, &prompt).unwrap();
        let session = db
            .record_session(chat, Agent::Claude, "first", &first, Some("startup"))
            .unwrap()
            .session_id;
        transcript::read(&db, session).unwrap();
        // No status report triggers a read of the last answer.
        fs::write(
            &first,
            format!(
                "{prompt}{}",
                message("2", Some("1"), "assistant", "last answer")
            ),
        )
        .unwrap();
        fs::write(&second, message("3", None, "user", "second prompt")).unwrap();
        let (mut reports, _) = Reports::new();
        // This session switch is still queued when the workspace is released.
        fs::write(
            reports.path("cli"),
            format!(
                "{}\n",
                json!({
                    "session_id": "second", "transcript_path": second, "source": "clear"
                })
            ),
        )
        .unwrap();

        import_chat(&db, &mut reports, chat, Agent::Claude, "cli", Vec::new()).unwrap();
        assert!(reports.read("cli", Agent::Claude).is_empty());
        reports.remove();
        // The original transcripts can expire; the database still has both.
        fs::remove_file(&first).unwrap();
        fs::remove_file(&second).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let sessions = db.sessions(chat).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|session| session.prompted));
        assert_eq!(
            db.items(sessions[0].id, 100).unwrap()[1].payload["text"],
            "last answer"
        );
        assert_eq!(
            db.items(sessions[1].id, 100).unwrap()[0].payload["text"],
            "second prompt"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shutdown_continues_importing_after_one_transcript_fails() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = Db::open_in_memory().unwrap();
        let project = db.add_project(&dir).unwrap();
        let chat = db.add_chat(project, Agent::Claude).unwrap();
        // A directory cannot be read as a transcript.
        let bad = db
            .record_session(chat, Agent::Claude, "bad", &dir, None)
            .unwrap()
            .session_id;
        let good = dir.join("good.jsonl");
        fs::write(&good, message("1", None, "user", "kept")).unwrap();
        let session = db
            .record_session(chat, Agent::Claude, "good", &good, None)
            .unwrap()
            .session_id;
        let (mut reports, _) = Reports::new();

        let sessions = vec![bad, session];
        assert!(import_chat(&db, &mut reports, chat, Agent::Claude, "cli", sessions).is_err());
        assert_eq!(db.items(session, 100).unwrap()[0].payload["text"], "kept");
        reports.remove();
        fs::remove_dir_all(dir).unwrap();
    }
}
