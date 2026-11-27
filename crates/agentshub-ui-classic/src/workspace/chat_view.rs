//! The shown chat's body, the empty state and the shortcut list, built from
//! what the workspace holds.

use agentshub_ui_kit::chat_body::{ChatBody, ChatIntent, Shell};
use agentshub_ui_kit::empty::{EmptyIntent, EmptyState, Recent};
use agentshub_ui_kit::settings::SHORTCUTS_WORK_ANYWHERE;

use super::*;
use agentshub_ui_kit::shortcuts::render_shortcuts;

impl Workspace {
    /// What the workspace holds of chat `id`, which shows, unless it has
    /// gone.
    pub(super) fn shown_chat(&self, id: i64, cx: &Context<Self>) -> Option<ShownChat> {
        let runtime = self.runtime.read(cx);
        let (Some(chat), Some(pane)) = (runtime.chat(id), self.panes.get(&id)) else {
            return None;
        };
        let history = pane.history.read(cx);
        let (earlier, history_open) = (history.count(), history.is_open());
        let shell = self.shown_shell(cx).map(|(cwd, view)| Shell { cwd, view });
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
        Some(ShownChat {
            focus: self.chat_focus.clone(),
            earlier,
            history_open,
            body: ChatBody {
                id: chat.id,
                notice: chat.notice.cloned(),
                message: chat.message.cloned(),
                history: pane.history.clone(),
                terminal: pane.view.clone(),
                shell,
                shell_share: cx.global::<Settings>().shell_share,
                active: runtime.active().is_some(),
                on,
                save,
            },
        })
    }

    pub(super) fn render_empty_over_shell(
        &self,
        above: impl IntoElement,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let shell = self.shown_shell(cx).map(|(cwd, view)| Shell { cwd, view });
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
        agentshub_ui_kit::chat_body::over_shell(
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
            ChatIntent::OpenEditor(editor) => self.apply(
                Intent::OpenInEditor {
                    chat: Some(id),
                    editor: Some(editor),
                },
                cx,
            ),
            ChatIntent::SetEditor(editor) => {
                let saved = Settings::set_editor(editor, cx);
                self.save_setting(saved, cx);
            }
        }
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
        let project = self.layout.selected_project(cx)?;
        let (id, agents) = self.runtime.read(cx).installed()?;
        if id != project {
            return None;
        }
        Some((project, agents.to_vec()))
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
        let runtime = self.runtime.read(cx);
        let selected = self.layout.selected_project(cx).and_then(|id| {
            runtime
                .projects()
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.name.clone().into())
        });
        let agents = self.empty_agents(cx);
        let recent: Vec<_> = {
            let running = agentshub_ui_kit::layout::running(runtime);
            let mut recent: Vec<_> = runtime
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
                    let tone = Tone::of(running.get(&chat.id).copied());
                    Recent {
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
                move |intent: EmptyIntent, window: &mut Window, cx: &mut App| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.empty_intent(intent, window, cx));
                    }
                },
            )
        };
        EmptyState {
            selected,
            agents,
            recent,
            on,
        }
        .render(window, cx)
    }

    fn empty_intent(&mut self, intent: EmptyIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            EmptyIntent::StartAgent(project, agent) => {
                self.apply(Intent::StartAgent { project, agent }, cx)
            }
            EmptyIntent::NewChat => self.new_chat_here(&NewChat, window, cx),
            EmptyIntent::AddProject => self.add_project(window, cx),
            EmptyIntent::OpenChat(id) => self.open_chat(id, window, cx),
        }
    }
}
