use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::Agent;
use crate::appearance;
use crate::reports::{self, Reports};
use crate::sessions::{self, Session};
use crate::sidebar::{NewChat, Sidebar, SidebarEvent, lamp};
use crate::terminal::{self, Exited, Pty, Terminal};
use crate::terminal_view::TerminalView;

const SIDEBAR_WIDTH: Pixels = px(280.);

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
            .child(lamp(appearance::agent_color(*self, cx), false))
            .child(self.display_name())
    }
}

pub struct Workspace {
    sidebar: Entity<Sidebar>,
    /// Open terminals, in the order they were started. Their processes keep
    /// running while another chat is shown.
    chats: Vec<Chat>,
    /// The key of the chat that is shown.
    active: Option<String>,
    reports: Reports,
    _sidebar_events: Subscription,
    _report_events: Task<()>,
    _appearance: Subscription,
    _theme: Subscription,
    new_session_select: Option<Subscription>,
}

struct Chat {
    key: String,
    agent: Agent,
    cwd: PathBuf,
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    /// The session file the CLI is in, once known. It changes when the CLI
    /// switches to another session, as on `/clear`.
    session: Option<PathBuf>,
    _exited: Subscription,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(Sidebar::new);
        let sidebar_events =
            cx.subscribe_in(&sidebar, window, |this, _, event, window, cx| match event {
                SidebarEvent::OpenSession(session) => this.open_session(session, window, cx),
                SidebarEvent::ShowChat(key) => this.activate(key.clone(), window, cx),
                SidebarEvent::NewSession(project) => {
                    this.open_new_session_dialog(project.clone(), window, cx)
                }
                SidebarEvent::SessionsChanged => this.find_codex_sessions(cx),
            });

        let (reports, reported) = Reports::new();
        let report_events = cx.spawn(async move |this, cx| {
            while let Ok(key) = reported.recv().await {
                if this
                    .update(cx, |this, cx| this.read_report(&key, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        // Closing the window releases the workspace before the app quits, and
        // quitting releases it too.
        cx.on_release(|this, cx| {
            this.reports.remove();
            // The CLIs are killed on threads of their own, which the app would
            // otherwise exit before they are done with.
            let reaped: Vec<_> = this
                .chats
                .iter()
                .map(|chat| chat.terminal.read(cx).shutdown())
                .collect();
            terminal::wait_reaped(reaped);
        })
        .detach();

        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx)
        });
        let theme = cx.observe_global::<Theme>(|this, cx| {
            let colors = appearance::terminal_colors(cx);
            for chat in &this.chats {
                chat.terminal.read(cx).set_colors(colors);
            }
        });

        Self {
            sidebar,
            chats: Vec::new(),
            active: None,
            reports,
            _sidebar_events: sidebar_events,
            _report_events: report_events,
            _appearance: appearance,
            _theme: theme,
            new_session_select: None,
        }
    }

    fn open_session(&mut self, session: &Session, window: &mut Window, cx: &mut Context<Self>) {
        let running = self.chats.iter().find(|chat| {
            chat.session.as_ref() == Some(&session.path) && !chat.terminal.read(cx).exited()
        });
        if let Some(chat) = running {
            self.activate(chat.key.clone(), window, cx);
        } else {
            let args = session.agent.resume_args(&session.id, &session.path);
            self.open_terminal(
                session.agent,
                &args,
                &session.cwd,
                Some(session.path.clone()),
                window,
                cx,
            );
        }
    }

    fn open_new_session_dialog(
        &mut self,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let select = cx.new(|cx| SelectState::new(Agent::ALL.to_vec(), None, window, cx));
        let project_label = SharedString::from(project.to_string_lossy().into_owned());
        self.new_session_select =
            Some(
                cx.subscribe_in(&select, window, move |this, _, event, window, cx| {
                    if let SelectEvent::Confirm(Some(agent)) = event {
                        window.close_dialog(cx);
                        this.open_terminal(*agent, &[], &project, None, window, cx);
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

    fn read_report(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.iter_mut().find(|chat| chat.key == key) else {
            return;
        };
        let session = self.reports.read(key);
        if session.is_some() && chat.session != session {
            chat.session = session;
            self.sync_sidebar(cx);
        }
    }

    /// Codex writes its session files as it goes, so the sessions of its
    /// terminals are looked for whenever those change.
    fn find_codex_sessions(&mut self, cx: &mut Context<Self>) {
        let hidden = self.sidebar.read(cx).hidden_sessions();
        for chat in &mut self.chats {
            if chat.agent != Agent::Codex {
                continue;
            }
            let terminal = chat.terminal.read(cx);
            if let Some(session) = terminal.with_pid(|pid| sessions::codex_session_of(pid, hidden))
            {
                chat.session = Some(session);
            }
        }
        self.sync_sidebar(cx);
    }

    /// Lists running chats whose session isn't listed yet as new chats, and
    /// marks the one that is shown.
    fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        let listed: HashMap<&Path, &str> = self
            .sidebar
            .read(cx)
            .sessions()
            .map(|session| (session.path.as_path(), session.id.as_str()))
            .collect();
        let mut new_chats = Vec::new();
        let mut running = HashSet::new();
        let mut active = None;
        for chat in &self.chats {
            let id = match chat.session.as_deref().and_then(|path| listed.get(path)) {
                Some(id) => id.to_string(),
                None => {
                    if !chat.terminal.read(cx).exited() {
                        new_chats.push(NewChat {
                            key: chat.key.clone(),
                            agent: chat.agent,
                            cwd: chat.cwd.clone(),
                        });
                    }
                    chat.key.clone()
                }
            };
            if self.active.as_ref() == Some(&chat.key) {
                active = Some(id.clone());
            }
            if !chat.terminal.read(cx).exited() {
                running.insert(id);
            }
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_chats(new_chats, running, active, cx)
        });
    }

    fn open_terminal(
        &mut self,
        agent: Agent,
        args: &[String],
        cwd: &Path,
        session: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = uuid::Uuid::new_v4().to_string();
        let colors = appearance::terminal_colors(cx);
        let mut all_args = self.reports.args(agent);
        all_args.extend_from_slice(args);
        let report = self.reports.path(&key);
        let env = [(reports::ENV, report.as_os_str())];
        let pty = match Pty::spawn(agent.program(), &all_args, &env, cwd, colors) {
            Ok(pty) => pty,
            Err(err) => {
                window.push_notification(format!("Couldn't start {}: {err}", agent.program()), cx);
                return;
            }
        };
        let terminal = cx.new(|cx| Terminal::new(pty, cx));
        let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
        let exited = cx.subscribe(&terminal, {
            let key = key.clone();
            move |this, _, _: &Exited, cx| {
                this.chats.retain(|chat| chat.key != key);
                if this.active.as_ref() == Some(&key) {
                    this.active = None;
                }
                this.sync_sidebar(cx);
                cx.notify();
            }
        });
        self.chats.push(Chat {
            key: key.clone(),
            agent,
            cwd: cwd.to_path_buf(),
            terminal,
            view,
            session,
            _exited: exited,
        });
        self.activate(key, window, cx);
    }

    fn activate(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        for chat in &self.chats {
            let shown = chat.key == key;
            chat.terminal.read(cx).set_visible(shown);
            if shown {
                chat.view.focus_handle(cx).focus(window, cx);
            }
        }
        self.active = Some(key);
        self.sync_sidebar(cx);
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let active_terminal = self
            .active
            .as_ref()
            .and_then(|key| self.chats.iter().find(|chat| &chat.key == key))
            .map(|chat| &chat.view);

        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            // Terminal output re-renders the window many times a second; the
            // sidebar only needs to render again when its own state changes.
            .child(
                self.sidebar.clone().cached(
                    StyleRefinement::default()
                        .w(SIDEBAR_WIDTH)
                        .h_full()
                        .flex_none(),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .map(|this| match active_terminal {
                        Some(view) => this.child(view.clone()),
                        None => this.child(render_empty(cx)),
                    }),
            )
    }
}

/// What shows while no chat is open: how to open one, and how to read the
/// sidebar's lamps.
fn render_empty(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let legend_item =
        |color, lit, label: &'static str| h_flex().gap_2().child(lamp(color, lit)).child(label);

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
                                .child("Resume a session, or start a new one"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .line_height(rems(1.4))
                                .text_color(theme.muted_foreground)
                                .child(
                                    "Choose a session on the left to reopen it. To start \
                                     Claude Code, Codex or Pi in a project, press + beside \
                                     it. Agents keep running while you look at another \
                                     session.",
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
                                false,
                                agent.display_name(),
                            )
                        })))
                        .child(
                            h_flex()
                                .gap_5()
                                .text_color(theme.muted_foreground)
                                .child(legend_item(theme.muted_foreground, false, "Saved"))
                                .child(legend_item(theme.muted_foreground, true, "Running")),
                        ),
                ),
        )
}
