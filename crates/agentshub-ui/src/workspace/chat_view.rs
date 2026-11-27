//! The chat sheet's remaining chrome: toolbar, footer and empty state.

use crate::chat::{ChatIntent, ChatSheet};
use crate::settings::SHORTCUTS_WORK_ANYWHERE;

use super::render::render_shortcuts;
use super::*;

impl Workspace {
    pub(super) fn render_chat(&self, id: i64, window: &Window, cx: &Context<Self>) -> AnyElement {
        let runtime = self.runtime.read(cx);
        let (Some(chat), Some(pane)) = (runtime.chat(id), self.panes.get(&id)) else {
            return Empty.into_any_element();
        };
        let (title, pinned) = self
            .active_entry(cx)
            .map(|entry| {
                (
                    entry.title.clone().unwrap_or_else(|| "New chat".into()),
                    entry.pinned,
                )
            })
            .unwrap_or_else(|| ("New chat".into(), false));
        let history = pane.history.read(cx);
        let (earlier, history_open) = (history.count(), history.is_open());
        let shell = self
            .shown_shell(cx)
            .map(|(cwd, view)| crate::chat::Shell { cwd, view });
        let on = {
            let this = cx.weak_entity();
            let id = chat.id;
            Rc::new(
                move |intent: ChatIntent, window: &mut Window, cx: &mut App| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.chat_intent(id, intent, window, cx));
                    }
                },
            )
        };
        let save = {
            let this = cx.weak_entity();
            Rc::new(move |cx: &mut App| {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| this.save_shell_share(cx));
                }
            })
        };
        ChatSheet {
            id: chat.id,
            title: title.into(),
            pinned,
            agent: chat.agent,
            cwd: chat.cwd.to_path_buf(),
            tone: Tone::of(self.sidebar.read(cx).running(chat.id)),
            turn: chat.turn,
            running: chat.running,
            notice: chat.notice.cloned(),
            message: chat.message.cloned(),
            history: pane.history.clone(),
            terminal: pane.view.clone(),
            shell,
            shell_share: cx.global::<Settings>().shell_share,
            hidden: self.hidden,
            active: runtime.active().is_some(),
            earlier,
            history_open,
            editors: crate::chat::editor_choices(cx),
            focus: self.chat_focus.clone(),
            on,
            save,
        }
        .render(window, cx)
    }

    pub(super) fn render_empty_over_shell(
        &self,
        above: impl IntoElement,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let shell = self
            .shown_shell(cx)
            .map(|(cwd, view)| crate::chat::Shell { cwd, view });
        let on = {
            let this = cx.weak_entity();
            Rc::new(
                move |intent: ChatIntent, window: &mut Window, cx: &mut App| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.chat_intent(0, intent, window, cx));
                    }
                },
            )
        };
        let save = {
            let this = cx.weak_entity();
            Rc::new(move |cx: &mut App| {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| this.save_shell_share(cx));
                }
            })
        };
        crate::chat::over_shell(
            above,
            px(0.),
            shell,
            cx.global::<Settings>().shell_share,
            true,
            on,
            save,
            window,
            cx,
        )
    }

    fn chat_intent(
        &mut self,
        id: i64,
        intent: ChatIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            ChatIntent::Restart => self.apply(Intent::RestartChat(id), cx),
            ChatIntent::ToggleHistory => self.toggle_history(&ToggleHistory, window, cx),
            ChatIntent::ToggleShell => self.toggle_shell(&ToggleShell, window, cx),
            ChatIntent::ResizeShell(share) => self.resize_shell(share, cx),
            ChatIntent::OpenEditor(editor) => self.open_chat_in(editor, id, cx),
            ChatIntent::SetEditor(editor) => {
                let saved = Settings::set_editor(editor, cx);
                self.save_setting(saved, cx);
            }
        }
    }

    /// The search, which opens the search modal, and the menu of the running
    /// chats, over the `width` the sidebar leaves.
    pub(super) fn render_toolbar(
        &self,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let running = self
            .sidebar
            .read(cx)
            .running_chats()
            .into_iter()
            .map(|(_, _, tone)| tone)
            .collect();
        let on = {
            let this = cx.weak_entity();
            Rc::new(
                move |intent: crate::chat::ToolbarIntent, window: &mut Window, cx: &mut App| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.toolbar_intent(intent, window, cx));
                    }
                },
            )
        };
        crate::chat::Toolbar {
            width,
            running,
            menu: self.running_menu.clone(),
            on,
        }
        .render(window, cx)
    }

    fn toolbar_intent(
        &mut self,
        intent: crate::chat::ToolbarIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            crate::chat::ToolbarIntent::OpenPalette => self.open_palette(false, window, cx),
            crate::chat::ToolbarIntent::RunningMenu(open) => {
                if open {
                    self.open_running_menu(window, cx);
                    if let Some(menu) = &self.running_menu {
                        menu.focus_handle(cx).focus(window, cx);
                    }
                } else {
                    self.running_menu = None;
                    cx.notify();
                }
            }
            crate::chat::ToolbarIntent::CloseWindow => {
                crate::tray::remember_bounds(window.window_bounds(), cx);
                window.remove_window();
            }
        }
    }

    pub(super) fn render_footer(
        &self,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let sidebar = self.sidebar.read(cx);
        let summary = sidebar.summary();
        let needing = sidebar.next_needing_you(None).is_some();
        crate::chat::Footer {
            summary: &summary,
            needing,
            width,
        }
        .render(window, cx)
    }

    pub(super) fn show_shortcuts(
        &mut self,
        _: &ShowShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.open_dialog(cx, |dialog, window, cx| {
            dialog.title("Keyboard shortcuts").w(px(640.)).child(
                v_flex()
                    .id("shortcuts")
                    .max_h(px(480.))
                    .overflow_y_scroll()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .line_height(rems(1.4))
                            .text_color(cx.theme().muted_foreground)
                            .child(SHORTCUTS_WORK_ANYWHERE),
                    )
                    .child(render_shortcuts(window, cx)),
            )
        });
    }

    /// The agents the selected project can start now, in the new chat
    /// dialog's order.
    fn empty_agents(&self, cx: &App) -> Option<(i64, Vec<Agent>)> {
        let project = self.sidebar.read(cx).selected()?;
        let (id, agents) = self.runtime.read(cx).installed()?;
        if id != project.id {
            return None;
        }
        Some((project.id, agents.to_vec()))
    }

    /// Starts the `n`th agent the empty state offers, counting from one, on
    /// its number key.
    pub(super) fn empty_key(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.runtime.read(cx).active().is_some()
            || self.settings_shown
            || event.keystroke.modifiers.number_of_modifiers() > 0
        {
            return;
        }
        let Some(n) = event.keystroke.key.parse::<usize>().ok() else {
            return;
        };
        if let Some((project, agents)) = self.empty_agents(cx)
            && let Some(agent) = n.checked_sub(1).and_then(|n| agents.get(n))
        {
            cx.stop_propagation();
            self.apply(
                Intent::StartAgent {
                    project,
                    agent: *agent,
                },
                cx,
            );
        }
    }

    /// What shows while no chat is open: the agents to start, with their
    /// number keys, and the chats last worked in, to go back to.
    pub(super) fn render_empty(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let selected = self
            .sidebar
            .read(cx)
            .selected()
            .map(|project| project.name.clone().into());
        let agents = self.empty_agents(cx);
        let recent: Vec<_> = {
            let sidebar = self.sidebar.read(cx);
            let mut recent: Vec<_> = sidebar
                .projects()
                .iter()
                .filter(|project| project.archived_at.is_none())
                .flat_map(|project| {
                    project
                        .chats
                        .iter()
                        .filter(|chat| chat.archived_at.is_none())
                        .map(move |chat| (project, chat))
                })
                .collect();
            recent.sort_by_key(|(_, chat)| std::cmp::Reverse(chat.updated_at));
            recent
                .into_iter()
                .take(RECENT_CHATS)
                .map(|(project, chat)| {
                    let tone = Tone::of(sidebar.running(chat.id));
                    crate::chat::Recent {
                        id: chat.id,
                        title: chat.title.clone().unwrap_or_else(|| "New chat".into()),
                        about: sidebar::subtitle(chat, tone, Some(&project.name)),
                        tone,
                    }
                })
                .collect()
        };
        let on = {
            let this = cx.weak_entity();
            Rc::new(
                move |intent: crate::chat::EmptyIntent, window: &mut Window, cx: &mut App| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.empty_intent(intent, window, cx));
                    }
                },
            )
        };
        crate::chat::EmptyState {
            selected,
            agents,
            recent,
            on,
        }
        .render(window, cx)
    }

    fn empty_intent(
        &mut self,
        intent: crate::chat::EmptyIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            crate::chat::EmptyIntent::StartAgent(project, agent) => {
                self.apply(Intent::StartAgent { project, agent }, cx)
            }
            crate::chat::EmptyIntent::NewChat => self.new_chat_here(&NewChat, window, cx),
            crate::chat::EmptyIntent::AddProject => self.add_project(window, cx),
            crate::chat::EmptyIntent::OpenChat(id) => self.open_chat(id, window, cx),
        }
    }
}
