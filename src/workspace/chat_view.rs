//! The chat sheet's remaining chrome: toolbar, footer and empty state.

use agentshub_ui::chat::{ChatIntent, ChatSheet};
use agentshub_ui::settings::SHORTCUTS_WORK_ANYWHERE;

use super::render::{render_shortcuts, stay_put};
use super::*;

impl Workspace {
    pub(super) fn render_chat(
        &self,
        chat: &Chat,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (title, pinned) = self
            .active_entry(cx)
            .map(|entry| {
                (
                    entry.title.clone().unwrap_or_else(|| "New chat".into()),
                    entry.pinned,
                )
            })
            .unwrap_or_else(|| ("New chat".into(), false));
        let history = chat.pane.history.read(cx);
        let (earlier, history_open) = (history.count(), history.is_open());
        let shell = self
            .shown_shell(cx)
            .map(|(cwd, shell)| agentshub_ui::chat::Shell {
                cwd,
                view: shell.view.clone(),
            });
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
            cwd: chat.cwd.clone(),
            tone: Tone::of(self.sidebar.read(cx).running(chat.id)),
            turn: chat.turn,
            running: chat.running(),
            notice: chat.notice.clone(),
            message: chat.message.clone(),
            history: chat.pane.history.clone(),
            terminal: chat
                .pane
                .terminal
                .as_ref()
                .map(|terminal| terminal.view.clone()),
            shell,
            shell_share: cx.global::<Settings>().shell_share,
            hidden: self.hidden,
            active: self.active.is_some(),
            earlier,
            history_open,
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
            .map(|(cwd, shell)| agentshub_ui::chat::Shell {
                cwd,
                view: shell.view.clone(),
            });
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
        agentshub_ui::chat::over_shell(
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
            ChatIntent::Restart => self.restart(id, window, cx),
            ChatIntent::ToggleHistory => self.toggle_history(&ToggleHistory, window, cx),
            ChatIntent::ToggleShell => self.toggle_shell(&ToggleShell, window, cx),
            ChatIntent::ResizeShell(share) => self.resize_shell(share, cx),
        }
    }

    /// The search, which opens the search modal, and the menu of the running
    /// chats, over the `width` the sidebar leaves.
    pub(super) fn render_toolbar(
        &self,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let sidebar = self.sidebar.read(cx);
        let running: Vec<_> = sidebar
            .running_chats()
            .into_iter()
            .map(|(_, _, tone)| tone)
            .collect();
        let needing = running.iter().filter(|tone| tone.needs_you()).count();
        let working = running.contains(&Tone::Working);
        let count = match (needing, running.len()) {
            (0, 0) => None,
            (0, open) => Some(sidebar::badge(open, false, working, cx)),
            (needing, _) => Some(sidebar::badge(needing, true, false, cx)),
        };
        let menu = self.running_menu.clone();
        let workspace = cx.weak_entity();

        let search = Button::new("search")
            .outline()
            .flex_1()
            .min_w_0()
            .h_8()
            .bg(theme.background)
            .icon(IconName::Search)
            .accessibility_label("Search chats, messages, projects and commands")
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child("Search chats, messages, projects and commands"),
                    )
                    // Too narrow for both, the search keeps its name.
                    .children(
                        palette::shortcut(&OpenSearch, window)
                            .filter(|_| width >= MIN_SEARCH_KEYS_WIDTH)
                            .map(|keys| {
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded(px(4.))
                                    .bg(palette.strong)
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(palette.secondary_foreground)
                                    .child(keys)
                            }),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(false, window, cx)));

        let running = Button::new("running")
            .ghost()
            .h_8()
            .icon(ExtraIcon::Activity)
            .label("Running")
            .children(count)
            .dropdown_caret(true)
            .tooltip_with_action("The running chats of every project", &ShowRunning, None);

        TitleBar::new()
            .h(TITLE_BAR_HEIGHT)
            .pl_2()
            .bg(transparent_black())
            .border_b_0()
            .on_close_window(|_, window, cx| {
                tray::remember_bounds(window.window_bounds(), cx);
                window.remove_window();
            })
            // Over the title bar's row rather than in it: the row never gets
            // narrower than what's in it, which would push the window's
            // controls out of a narrow window.
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .gap_4()
                    .pr_2()
                    .justify_between()
                    .child(stay_put(h_flex().flex_1().min_w_0().max_w(px(480.))).child(search))
                    .child(
                        stay_put(div()).child(
                            Popover::new("running-menu")
                                .anchor(Anchor::TopRight)
                                .appearance(false)
                                // The menu closes itself on a click outside it.
                                .overlay_closable(false)
                                .open(menu.is_some())
                                .when_some(menu.as_ref(), |this, menu| {
                                    this.track_focus(&menu.focus_handle(cx))
                                })
                                .on_open_change(move |open, window, cx| {
                                    workspace
                                        .update(cx, |this, cx| {
                                            if *open {
                                                // Opened from the button, the popover
                                                // took the focus before the menu was there.
                                                this.open_running_menu(window, cx);
                                                if let Some(menu) = &this.running_menu {
                                                    menu.focus_handle(cx).focus(window, cx);
                                                }
                                            } else {
                                                this.running_menu = None;
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                })
                                .trigger(running)
                                .content(move |_, _, _| match &menu {
                                    Some(menu) => menu.clone().into_any_element(),
                                    None => div().into_any_element(),
                                }),
                        ),
                    ),
            )
    }

    /// What the agents are doing, and the shortcuts that help with it now,
    /// over a sheet `width` wide. The rest are a shortcut away.
    pub(super) fn render_footer(
        &self,
        width: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let sidebar = self.sidebar.read(cx);
        let summary = sidebar.summary();
        let needing = sidebar.next_needing_you(None).is_some();
        agentshub_ui::chat::Footer {
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
        let (id, agents) = self.installed.as_ref()?;
        if *id != project.id {
            return None;
        }
        Some((project.id, agents.clone()))
    }

    /// Starts the `n`th agent the empty state offers, counting from one, on
    /// its number key.
    pub(super) fn empty_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.is_some()
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
            self.start_agent(project, *agent, window, cx);
        }
    }

    /// What shows while no chat is open: the agents to start, with their
    /// number keys, and the chats last worked in, to go back to.
    pub(super) fn render_empty(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let sidebar = self.sidebar.read(cx);
        let selected = sidebar.selected().map(|project| project.name.clone());
        let agents = self.empty_agents(cx);
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
        let recent: Vec<_> = recent
            .into_iter()
            .take(RECENT_CHATS)
            .map(|(project, chat)| {
                let tone = Tone::of(sidebar.running(chat.id));
                let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                (
                    chat.id,
                    title,
                    sidebar::subtitle(chat, tone, Some(&project.name)),
                    tone,
                )
            })
            .collect();

        // Centered while it fits, and scrolled from the top when it doesn't.
        div().id("empty").size_full().overflow_y_scroll().child(
            h_flex()
                .min_h_full()
                .items_center()
                .justify_center()
                .p_8()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(520.))
                        .gap_6()
                        .child(
                            v_flex()
                                .gap_2()
                                .child(div().font_family(appearance::DISPLAY).text_3xl().child(
                                    match &selected {
                                        Some(_) => "Start an agent",
                                        None => "Add a project to begin",
                                    },
                                ))
                                .child(
                                    div()
                                        .text_sm()
                                        .line_height(rems(1.45))
                                        .text_color(theme.muted_foreground)
                                        .child(match &selected {
                                            Some(name) => format!(
                                                "In {name}. Agents keep running while you look at \
                                             another chat."
                                            ),
                                            None => "A project is a folder you work in. Add one, \
                                                 then start Claude Code, Codex or Pi there."
                                                .to_string(),
                                        }),
                                ),
                        )
                        .child(match (&selected, agents) {
                            (Some(_), Some((project, agents))) if !agents.is_empty() => h_flex()
                                .flex_wrap()
                                .gap_2()
                                .children(agents.into_iter().enumerate().map(|(index, agent)| {
                                    Button::new(("empty-agent", index as u64))
                                        .map(|button| {
                                            if index == 0 {
                                                button.primary()
                                            } else {
                                                button.outline()
                                            }
                                        })
                                        .icon(IconName::Plus)
                                        .label(agent.display_name())
                                        .child(
                                            div()
                                                .px_1p5()
                                                .rounded(px(4.))
                                                .bg(palette.strong)
                                                .text_xs()
                                                .font_family(theme.mono_font_family.clone())
                                                .text_color(palette.secondary_foreground)
                                                .child((index + 1).to_string()),
                                        )
                                        .tooltip(format!(
                                            "Start {} here, or press {}",
                                            agent.display_name(),
                                            index + 1
                                        ))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.start_agent(project, agent, window, cx)
                                        }))
                                }))
                                .child(
                                    Button::new("empty-new-chat")
                                        .ghost()
                                        .label("More options…")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_chat_here(&NewChat, window, cx)
                                        })),
                                )
                                .into_any_element(),
                            (Some(_), _) => h_flex()
                                .child(
                                    Button::new("empty-new-chat")
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label("New chat")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_chat_here(&NewChat, window, cx)
                                        })),
                                )
                                .into_any_element(),
                            (None, _) => h_flex()
                                .child(
                                    Button::new("empty-add-project")
                                        .primary()
                                        .icon(IconName::FolderOpen)
                                        .label("Add project")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.add_project(window, cx)
                                        })),
                                )
                                .into_any_element(),
                        })
                        .when(!recent.is_empty(), |this| {
                            this.child(
                                v_flex()
                                    .gap_1()
                                    .pt_5()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .pb_1()
                                            .child(sidebar::heading("Recent", cx))
                                            .children(palette::shortcut(&OpenSearch, window).map(
                                                |keys| {
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child(format!("{keys} to search them all"))
                                                },
                                            )),
                                    )
                                    .children(recent.into_iter().map(
                                        |(id, title, about, tone)| {
                                            h_flex()
                                                .id(("recent", id as u64))
                                                .px_2()
                                                .py(px(7.))
                                                .mx(px(-8.))
                                                .gap(px(10.))
                                                .rounded(theme.radius)
                                                .cursor_pointer()
                                                .hover(|style| style.bg(theme.accent))
                                                .child(sidebar::dot(tone, cx))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_sm()
                                                        .truncate()
                                                        .child(title),
                                                )
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .max_w(px(240.))
                                                        .truncate()
                                                        .text_xs()
                                                        .font_family(theme.mono_font_family.clone())
                                                        .text_color(tone.text_color(cx))
                                                        .child(about),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.open_chat(id, window, cx)
                                                    },
                                                ))
                                        },
                                    )),
                            )
                        }),
                ),
        )
    }
}
