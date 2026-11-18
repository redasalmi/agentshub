//! The chat list under the rail: filters, rows, archive and empty states.

use super::*;

impl Sidebar {
    pub(super) fn render_list(&self, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let id = project.id;
        let sidebar = cx.entity();
        let shown = self.shown();
        let archived = Self::archived(project);
        let has_chats = project.chats.len() > archived.len();
        let (pinned, others): (Vec<_>, Vec<_>) = shown
            .iter()
            .enumerate()
            .map(|(index, chat)| (index, *chat))
            .partition(|(_, chat)| chat.pinned);
        let pinned = self.rows(&pinned, true);
        let others = self.rows(&others, false);
        let quiet = pinned.is_empty() && others.is_empty();

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .flex_none()
                    .px_3()
                    .pt_4()
                    .pb_3()
                    .gap_3()
                    .child(
                        h_flex()
                            .group("project-header")
                            .items_start()
                            .gap_2()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .pl_1()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .truncate()
                                            .child(project.name.clone()),
                                    )
                                    .child(path_line(&project.path, cx)),
                            )
                            .child(
                                Button::new("project-menu")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Ellipsis)
                                    .tooltip("More")
                                    .dropdown_menu(move |menu, _, _| {
                                        let emit = |event: SidebarEvent| {
                                            let sidebar = sidebar.clone();
                                            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                                                sidebar.update(cx, |_, cx| cx.emit(event.clone()))
                                            }
                                        };
                                        menu.item(
                                            PopupMenuItem::new("Archive")
                                                .icon(ExtraIcon::Archive)
                                                .on_click(emit(SidebarEvent::ArchiveProject(id))),
                                        )
                                        .separator()
                                        .item(
                                            PopupMenuItem::new("Remove")
                                                .icon(IconName::Delete)
                                                .on_click(emit(SidebarEvent::RemoveProject(id))),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("new-chat")
                                    .primary()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip_with_action(
                                        "Start an agent here",
                                        &crate::workspace::NewChat,
                                        None,
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(SidebarEvent::NewChat(id))
                                    })),
                            ),
                    )
                    .child(self.render_filter(project, cx)),
            )
            .child(
                v_flex()
                    .id("chats")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .gap_px()
                    .when(!has_chats && quiet, |this| {
                        this.child(self.render_none(
                            if archived.is_empty() {
                                "No chats yet. Start an agent in this folder with New chat."
                            } else {
                                "No chats here. Start an agent in this folder with New chat."
                            },
                            cx,
                        ))
                    })
                    .when(has_chats && quiet, |this| {
                        this.child(self.render_none(
                            match self.filter {
                                Filter::NeedsYou => "No agent here needs you.",
                                _ => "No agent runs here.",
                            },
                            cx,
                        ))
                    })
                    .when(!pinned.is_empty(), |this| {
                        this.child(section("Pinned", cx))
                            .children(pinned.iter().map(|row| self.render_row(row, cx)))
                    })
                    .when(!others.is_empty(), |this| {
                        this.when(!pinned.is_empty(), |this| this.child(section("Chats", cx)))
                            .children(others.iter().map(|row| self.render_row(row, cx)))
                    })
                    .when(self.filter == Filter::All && !archived.is_empty(), |this| {
                        this.child(self.render_archived(&archived, cx))
                    }),
            )
    }

    /// The archived chats of a project, folded under a row that counts them.
    fn render_archived(&self, chats: &[&Chat], cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let expanded = self.show_archived;
        v_flex()
            .gap_px()
            .child(
                h_flex()
                    .id("archived")
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .gap_1()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme.foreground))
                    .child(Icon::new(IconName::ChevronRight).xsmall().with_spring(
                        "archived-chevron",
                        SpringAnimation::new(SLIDE).to(if expanded { 0.25 } else { 0. }),
                        // In radians, as the spring overshoots past a straight arrow, below
                        // the 0 that a percentage of a turn can't go under.
                        |chevron, turn| {
                            chevron.transform(Transformation::rotate(radians(
                                turn * std::f32::consts::TAU,
                            )))
                        },
                    ))
                    .child("Archived")
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(chats.len().to_string()),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_archived = !this.show_archived;
                        cx.notify();
                    })),
            )
            .when(expanded, |this| {
                this.children(chats.iter().enumerate().map(|(index, chat)| {
                    // Each one a little after the one above, so they read as
                    // unfolding from the heading.
                    let delay = STAGGER.as_secs_f32() * index.min(10) as f32;
                    let total = REVEAL.as_secs_f32() + delay;
                    self.render_archived_chat(chat, cx).with_animation(
                        ("archived-in", chat.id as u64),
                        Animation::new(Duration::from_secs_f32(total)),
                        move |row, delta| {
                            let shown =
                                ((delta * total - delay) / REVEAL.as_secs_f32()).clamp(0., 1.);
                            let shown = 1. - (1. - shown).powi(3);
                            row.opacity(shown).top(px(-6. * (1. - shown)))
                        },
                    )
                }))
            })
    }

    /// An archived chat, which opening brings back.
    fn render_archived_chat(&self, chat: &Chat, cx: &Context<Self>) -> ContextMenu<Stateful<Div>> {
        let theme = cx.theme();
        let id = chat.id;
        let sidebar = cx.entity();
        let group = SharedString::from(format!("archived-{id}"));
        let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
        let archived = chat.archived_at.map(age).unwrap_or_default();
        let row_bg = theme.sidebar_accent;

        h_flex()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex_none()
            .px_2()
            .py(px(7.))
            .gap(px(10.))
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(|style| style.bg(row_bg))
            .child(dot(Tone::Saved, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(format!("{} · archived {archived}", chat.agent.label())),
                    ),
            )
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_1()
                    .pl_2()
                    .items_center()
                    .bg(row_bg)
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        Button::new("restore-chat")
                            .ghost()
                            .xsmall()
                            .icon(ExtraIcon::ArchiveRestore)
                            .tooltip("Bring this chat back")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::RestoreChat(id));
                            })),
                    )
                    .child(
                        Button::new("delete-chat")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .tooltip("Delete this chat")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::DeleteChat(id));
                            })),
                    ),
            )
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::OpenChat(id))))
            .context_menu(move |menu, _, _| {
                let emit = |event| emit(&sidebar, event);
                menu.item(
                    PopupMenuItem::new("Open")
                        .icon(IconName::ExternalLink)
                        .on_click(emit(SidebarEvent::OpenChat(id))),
                )
                .item(
                    PopupMenuItem::new("Bring back")
                        .icon(ExtraIcon::ArchiveRestore)
                        .on_click(emit(SidebarEvent::RestoreChat(id))),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Delete")
                        .icon(IconName::Delete)
                        .on_click(emit(SidebarEvent::DeleteChat(id))),
                )
            })
    }

    fn render_filter(&self, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let segments = Filter::ALL.len() as f32;
        let at = Filter::ALL
            .iter()
            .position(|filter| *filter == self.filter)
            .unwrap_or_default() as f32;
        div()
            .p(px(3.))
            .rounded(theme.radius)
            .bg(palette.strong)
            .child(
                h_flex()
                    .relative()
                    // The thumb under the chosen filter, which slides to the
                    // one chosen next.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(relative(1. / segments))
                            .rounded(theme.radius - px(2.))
                            .bg(theme.background)
                            .shadow_xs()
                            .with_spring(
                                "filter-thumb",
                                SpringAnimation::new(SLIDE).to(at),
                                move |thumb, at| thumb.left(relative(at / segments)),
                            ),
                    )
                    .children(Filter::ALL.map(|filter| {
                        let active = self.filter == filter;
                        div()
                            .id(filter.label())
                            .flex_1()
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .text_xs()
                            .cursor_pointer()
                            .map(|this| {
                                if active {
                                    this.text_color(theme.foreground).font_medium()
                                } else {
                                    this.text_color(palette.secondary_foreground)
                                        .hover(|style| style.text_color(theme.foreground))
                                }
                            })
                            .child(filter.label())
                            .map(|this| match self.count(project, filter) {
                                0 => this,
                                count => this.child(
                                    div()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.muted_foreground)
                                        .child(count.to_string()),
                                ),
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)),
                            )
                    })),
            )
    }

    fn render_none(&self, text: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_4()
            .text_sm()
            .line_height(rems(1.4))
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    /// A chat in the list, `index` being its place in it.
    pub(super) fn render_chat(
        &self,
        index: usize,
        chat: &Chat,
        cx: &Context<Self>,
    ) -> ContextMenu<Stateful<Div>> {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let id = chat.id;
        let active = self.active == Some(id);
        let tone = Tone::of(self.running(id));
        let group = SharedString::from(format!("chat-{id}"));
        let title = SharedString::from(chat.title.clone().unwrap_or_else(|| "New chat".into()));
        let current_title = SharedString::from(chat.title.clone().unwrap_or_default());
        let pinned = chat.pinned;
        let open = tone != Tone::Saved;
        let order_group = (chat.project_id, pinned);
        let dragged = DraggedChat {
            id,
            index,
            group: order_group,
            title: title.clone(),
        };
        let drop_color = palette.accent;
        let row_bg = theme.sidebar_accent;
        let sidebar = cx.entity();
        let menu_title = current_title.clone();
        let meaning = SharedString::from(match tone {
            Tone::Saved => format!("{} isn't running", chat.agent.display_name()),
            _ => activity(chat.agent, self.running(id)),
        });
        let popped = self
            .popped
            .get(&id)
            .copied()
            .filter(|at| at.elapsed() < POP);

        h_flex()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex_none()
            .px_2()
            .py(px(7.))
            .gap(px(10.))
            .rounded(theme.radius)
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(row_bg)
                } else {
                    this.hover(|style| style.bg(row_bg))
                }
            })
            // On the list's edge, past its padding, as the projects' is.
            .when(active, |this| {
                this.child(
                    div()
                        .absolute()
                        .left(px(-8.))
                        .top(px(10.))
                        .bottom(px(10.))
                        .w(BAR_WIDTH)
                        .rounded_r(px(2.))
                        .bg(palette.accent),
                )
            })
            // What the glyph means, for the one the user doesn't know yet.
            .child(
                div()
                    .id(("tone", id as u64))
                    .child(popping_dot(tone, popped, cx))
                    .tooltip(move |window, cx| Tooltip::new(meaning.clone()).build(window, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .when(active, |this| this.font_medium())
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(tone.text_color(cx))
                            .truncate()
                            .child(subtitle(chat, tone, None)),
                    ),
            )
            .when(index < NUMBERED && self.held == Some(Held::Chats), |this| {
                this.child(key_number(index, cx))
            })
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_1()
                    .pl_2()
                    .items_center()
                    .bg(row_bg)
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        Button::new("pin-chat")
                            .ghost()
                            .xsmall()
                            .icon(if pinned {
                                ExtraIcon::PinOff
                            } else {
                                ExtraIcon::Pin
                            })
                            .tooltip(if pinned {
                                "Unpin this chat"
                            } else {
                                "Pin this chat to the top"
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::PinChat(id, !pinned));
                            })),
                    )
                    .child(
                        Button::new("rename-chat")
                            .ghost()
                            .xsmall()
                            .icon(ExtraIcon::Pencil)
                            .tooltip("Rename this chat")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::RenameChat(id, current_title.clone()));
                            })),
                    )
                    .when(open, |this| {
                        this.child(
                            Button::new("close-chat")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Close this chat")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(SidebarEvent::CloseChat(id));
                                })),
                        )
                    })
                    .child(
                        Button::new("archive-chat")
                            .ghost()
                            .xsmall()
                            .icon(ExtraIcon::Archive)
                            .tooltip("Archive this chat")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::ArchiveChat(id));
                            })),
                    ),
            )
            // Chats are reordered by dropping one on another of the same
            // project, pinned or not as it is.
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedChat>(move |style, dragged, _, _| {
                if dragged.group == order_group && dragged.id != id {
                    drop_line(style, dragged.index < index, drop_color)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |_, dragged: &DraggedChat, _, cx| {
                if dragged.group == order_group && dragged.id != id {
                    cx.emit(SidebarEvent::MoveChat(dragged.id, id));
                }
            }))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::OpenChat(id))))
            .context_menu(move |menu, _, _| {
                let emit = |event| emit(&sidebar, event);
                menu.item(
                    PopupMenuItem::new("Open")
                        .icon(IconName::ExternalLink)
                        .on_click(emit(SidebarEvent::OpenChat(id))),
                )
                .item(
                    PopupMenuItem::new("Rename…")
                        .icon(ExtraIcon::Pencil)
                        .on_click(emit(SidebarEvent::RenameChat(id, menu_title.clone()))),
                )
                .item(
                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin to the top" })
                        .icon(if pinned {
                            ExtraIcon::PinOff
                        } else {
                            ExtraIcon::Pin
                        })
                        .on_click(emit(SidebarEvent::PinChat(id, !pinned))),
                )
                .when(open, |menu| {
                    menu.item(
                        PopupMenuItem::new("Close")
                            .icon(IconName::Close)
                            .on_click(emit(SidebarEvent::CloseChat(id))),
                    )
                })
                .separator()
                .item(
                    PopupMenuItem::new("Archive")
                        .icon(ExtraIcon::Archive)
                        .on_click(emit(SidebarEvent::ArchiveChat(id))),
                )
                .item(
                    PopupMenuItem::new("Delete")
                        .icon(IconName::Delete)
                        .on_click(emit(SidebarEvent::DeleteChat(id))),
                )
            })
    }
}
