//! The chat sheet: its header, history, terminal, shell and empty state.

use agentshub_ui::settings::SHORTCUTS_WORK_ANYWHERE;

use super::render::{focus_bar, key_hint, render_shortcuts, status_pill, stay_put};
use super::*;

impl Workspace {
    pub(super) fn render_chat(
        &self,
        chat: &Chat,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let id = chat.id;
        let viewport = window.viewport_size().height;
        let shell_height = shell_height(cx.global::<Settings>().shell_share, viewport);
        let shell = self.shown_shell(cx);
        // Without a terminal, the history takes the sheet but for its header
        // and footer, and the shell.
        let history_max = if chat.pane.terminal.is_none() {
            viewport
                - px(140.)
                - if shell.is_some() {
                    shell_height
                } else {
                    px(0.)
                }
        } else {
            viewport * HISTORY_SHARE
        };

        // A spinner only earns its frames while the window is on screen. The
        // sidebar's glyphs stay still: each tick would rebuild the whole list.
        let spin = window.is_visible() && !self.hidden;
        // A window too short for even the least heights clips the chat
        // rather than drawing it under the footer.
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.chat_focus)
            .child(self.render_chat_header(chat, spin, cx))
            .when_some(chat.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(notice),
                )
            })
            .when_some(chat.message.clone(), |this, message| {
                let tint = appearance::palette(cx).danger;
                this.child(
                    h_flex()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .gap_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(tint.soft)
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(tint.text)
                                .child(message),
                        )
                        .child(
                            Button::new("restart-agent")
                                .small()
                                .label("Restart agent")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.restart(id, window, cx);
                                })),
                        ),
                )
            })
            .child(
                self.render_over_shell(
                    // The earlier sessions ask for a share of the window, but
                    // shrink to fit what the header, notices, shell and footer
                    // leave the CLI, which keeps its least height.
                    v_flex()
                        .size_full()
                        .children(
                            chat.pane
                                .history
                                .read(cx)
                                .height(history_max)
                                .map(|height| {
                                    chat.pane.history.clone().cached(
                                        StyleRefinement::default()
                                            .w_full()
                                            .h(height)
                                            .flex_shrink(1.)
                                            .min_h_0(),
                                    )
                                }),
                        )
                        .children(chat.pane.terminal.as_ref().map(|terminal| {
                            // With the shell beside it, which of the two has
                            // the keyboard.
                            let focused = shell.is_some()
                                && terminal.view.focus_handle(cx).is_focused(window);
                            div()
                                .relative()
                                .flex_1()
                                .min_h(MIN_TERMINAL_HEIGHT)
                                .pl_5()
                                .pr_2()
                                .pt_3()
                                .when(focused, |this| this.child(focus_bar(cx)))
                                .child(terminal.view.clone())
                        })),
                    if chat.pane.terminal.is_some() {
                        MIN_TERMINAL_HEIGHT
                    } else {
                        px(0.)
                    },
                    window,
                    cx,
                ),
            )
    }

    /// `above` filling what the shell leaves of the room, with the shell, when
    /// it shows, at the room's bottom. The shell keeps the height the user
    /// dragged it to, and what's above gives up its room to it but for
    /// `above_min`; only in a room too short for both does the shell shrink.
    pub(super) fn render_over_shell(
        &self,
        above: impl IntoElement,
        above_min: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_h_0()
            // Clips the shell as it slides in from the bottom.
            .overflow_hidden()
            // Follows the pointer anywhere in the window while the shell's
            // top edge is dragged. The shell grows up from the room's bottom,
            // which stays put however tall it is.
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<ResizeShell>, window, cx| {
                    let room = event.bounds.size.height - above_min;
                    let height = (event.bounds.bottom() - event.event.position.y).min(room);
                    let share = shell_share(height, window.viewport_size().height);
                    this.resize_shell(share, cx);
                },
            ))
            .child(div().flex_1().min_h(above_min).child(above))
            .children(self.render_shell(window, cx))
    }

    /// The shell, under the chat or what shows without one, when it shows.
    fn render_shell(&self, window: &Window, cx: &Context<Self>) -> Option<impl IntoElement> {
        let theme = cx.theme();
        let (cwd, shell) = self.shown_shell(cx)?;
        let viewport = window.viewport_size().height;
        let workspace = cx.weak_entity();
        let height = shell_height(cx.global::<Settings>().shell_share, viewport);
        let focused = shell.view.focus_handle(cx).is_focused(window);
        // Named after its folder, so each project's shell slides in when it
        // first shows.
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&cwd, &mut hasher);
        let key = std::hash::Hasher::finish(&hasher);
        Some(
            v_flex()
                .relative()
                .flex_shrink(1.)
                .h(height)
                .min_h(MIN_SHELL_HEIGHT)
                .child(
                    h_flex()
                        .flex_none()
                        .h_8()
                        .pl_5()
                        .pr_2()
                        .gap_2()
                        .bg(theme.muted)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_medium()
                                .text_color(theme.muted_foreground)
                                .child("Shell"),
                        )
                        .child(div().flex_1().min_w_0().child(sidebar::path_line(&cwd, cx)))
                        .child(
                            Button::new("close-shell")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip_with_action("Hide the shell", &ToggleShell, None)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_shell(&ToggleShell, window, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .pl_5()
                        .pr_2()
                        .when(focused && self.active.is_some(), |this| {
                            this.child(focus_bar(cx))
                        })
                        .child(shell.view.clone()),
                )
                // Last, so it lies over the header it edges. It resizes the
                // shell when dragged, saving its height once the drag ends,
                // and restores its default height when double clicked.
                .child(
                    resize_handle("shell-edge", Axis::Vertical)
                        .inside(HandleEdge::Leading)
                        .with_appearance(shell_edge_appearance(
                            appearance::palette(cx).border_strong,
                        ))
                        .on_drag(ResizeShell, move |drag, _, _, cx| {
                            cx.stop_propagation();
                            let workspace = workspace.clone();
                            cx.new(|cx| {
                                cx.on_release(move |_, cx| {
                                    workspace
                                        .update(cx, |this, cx| this.save_shell_share(cx))
                                        .ok();
                                })
                                .detach();
                                drag.clone()
                            })
                        })
                        .on_double_click(cx.listener(|this, _, _, cx| {
                            this.resize_shell(DEFAULT_SHELL_SHARE, cx);
                            this.save_shell_share(cx);
                        })),
                )
                // Up from the sheet's bottom edge, which clips it, into the
                // room it takes at once, so its terminal sizes only once.
                .with_animation(
                    ("shell-in", key),
                    Animation::new(SHELL_IN).with_easing(ease_out_quint()),
                    move |shell, shown| shell.top(height * (1. - shown)),
                ),
        )
    }

    /// The chat's title and folder, what its agent is doing, and what can be
    /// done with it.
    fn render_chat_header(&self, chat: &Chat, spin: bool, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let entry = self.active_entry(cx);
        let title = entry
            .and_then(|entry| entry.title.clone())
            .unwrap_or_else(|| "New chat".into());
        let pinned = entry.is_some_and(|entry| entry.pinned);
        let tone = Tone::of(self.sidebar.read(cx).running(chat.id));
        let history = chat.pane.history.read(cx);
        let (earlier, open) = (history.count(), history.is_open());
        let focus = self.focus_handle.clone();

        // A sheet too narrow for the title, the status and the actions on one
        // line wraps them, rather than pushing the actions out of reach.
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap_x_4()
            .gap_y_2()
            .pl_6()
            .pr_3()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(MIN_TITLE_WIDTH)
                    .child(
                        div()
                            .font_family(appearance::DISPLAY)
                            .text_xl()
                            .truncate()
                            .child(title),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex_none()
                                    .child(format!("{} · ", chat.agent.label())),
                            )
                            .child(sidebar::path_line(&chat.cwd, cx)),
                    ),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .flex_wrap()
                    .justify_end()
                    .gap_x_4()
                    .gap_y_2()
                    .when(chat.running(), |this| {
                        this.child(status_pill(tone, chat.turn, spin, cx))
                    })
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when(earlier > 0, |this| {
                                this.child(
                                    Button::new("history")
                                        .ghost()
                                        .small()
                                        .icon(ExtraIcon::Clock)
                                        .label(format!("{earlier} earlier"))
                                        .selected(open)
                                        .tooltip_with_action(
                                            "Show or hide the earlier sessions",
                                            &ToggleHistory,
                                            None,
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_history(&ToggleHistory, window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("shell")
                                    .ghost()
                                    .small()
                                    .icon(IconName::SquareTerminal)
                                    .label("Shell")
                                    .selected(self.shell_visible(cx))
                                    .tooltip_with_action(
                                        "Show or hide the project's shell",
                                        &ToggleShell,
                                        None,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_shell(&ToggleShell, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("chat-menu")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Ellipsis)
                                    .tooltip("More")
                                    .dropdown_menu(move |menu, _, _| {
                                        menu.action_context(focus.clone())
                                            .menu("Rename", Box::new(RenameChat))
                                            .menu(
                                                if pinned { "Unpin" } else { "Pin to the top" },
                                                Box::new(TogglePin),
                                            )
                                            .separator()
                                            .menu("Archive", Box::new(ArchiveChat))
                                            .menu("Delete", Box::new(DeleteChat))
                                    }),
                            ),
                    ),
            )
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
    ) -> impl IntoElement {
        let theme = cx.theme();
        let sidebar = self.sidebar.read(cx);
        let summary = sidebar.summary();
        let needing = sidebar.next_needing_you(None).is_some();
        let bound = |action: &dyn Action, label: &'static str| {
            palette::shortcut(action, window).map(|keys| key_hint(keys, label, cx))
        };

        h_flex()
            .flex_none()
            .h_8()
            .px_4()
            .gap_4()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .rounded_b(theme.radius_lg - px(1.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            // What the agents do comes first and is cut last; the hints
            // drop out whole rather than being cut mid-word.
            .child(div().flex_1().min_w_0().truncate().child(summary))
            .when(needing, |this| {
                this.children(bound(&NextNeedingYou, "next that needs you"))
            })
            .when(width >= MIN_FOOTER_SEARCH_WIDTH, |this| {
                this.children(bound(&OpenSearch, "search"))
            })
            .children(bound(&ShowShortcuts, "shortcuts"))
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
