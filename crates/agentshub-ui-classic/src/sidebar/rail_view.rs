//! The project rail: tiles, names, selection bar and add button.

use super::*;

impl Sidebar {
    pub(super) fn render_rail(&self, window_width: Pixels, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (named, width) = rail(window_width, cx);
        let marks = marks(self.listed().map(|project| project.name.as_str()));
        let settings = Button::new("settings")
            .ghost()
            .icon(ExtraIcon::Settings)
            .tooltip("Settings")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::OpenSettings)));
        let toggle = Button::new("project-names")
            .ghost()
            .icon(if named {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeftOpen
            })
            .tooltip(if named {
                "Hide the projects' names"
            } else {
                "Show the projects' names"
            })
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::ShowProjectNames(!named))),
            );

        v_flex()
            .w(width)
            .h_full()
            .flex_none()
            .py_3()
            .gap_2()
            .border_r_1()
            .border_color(theme.sidebar_border)
            .when(named, |this| {
                this.child(heading("Projects", cx).px_3().pt_1())
            })
            .child(
                v_flex()
                    .id("project-tiles")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .map(|this| {
                        if named {
                            this.px(px(6.)).gap_px()
                        } else {
                            // Room for the badges over the tiles' corners.
                            this.items_center().gap_2().pt_1()
                        }
                    })
                    .relative()
                    .children(self.listed().zip(marks).enumerate().map(
                        |(index, (project, mark))| {
                            self.render_project(index, project, mark.into(), named, cx)
                        },
                    ))
                    .child(self.render_add_project(named, cx))
                    .children(self.render_selection_bar(named, cx)),
            )
            .child(if named {
                h_flex()
                    .px_2()
                    .justify_between()
                    .child(settings)
                    .child(toggle)
            } else {
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(toggle)
                    .child(settings)
            })
    }

    /// The bar on the rail's edge beside the selected project, which slides to
    /// the project selected next.
    fn render_selection_bar(&self, named: bool, cx: &Context<Self>) -> Option<impl IntoElement> {
        let index = self
            .listed()
            .position(|project| Some(project.id) == self.selected)?;
        // Where the rows or tiles are, as the list lays them out, and the
        // bar's length beside one.
        let (first, pitch, inset, left) = if named {
            (px(0.), ROW_HEIGHT + px(1.), px(9.), px(0.))
        } else {
            (px(4.), TILE_SIZE + px(8.), px(10.), px(1.))
        };
        let length = if named { ROW_HEIGHT } else { TILE_SIZE } - inset * 2.;
        let top = first + pitch * index as f32 + inset;
        Some(
            div()
                .absolute()
                .left(left)
                .w(BAR_WIDTH)
                .h(length)
                .rounded_r(px(2.))
                .bg(appearance::palette(cx).accent)
                // Its own spring for each layout, so it doesn't fly between
                // them when the rail folds.
                .with_spring(
                    ("selection-bar", named as u64),
                    SpringAnimation::new(SLIDE).to(top),
                    |bar, top| bar.top(top),
                ),
        )
    }

    /// A project in the rail, with how many of its chats need the user: a
    /// row with its mark and its name, or a tile with its mark alone.
    #[expect(
        clippy::too_many_lines,
        reason = "one element tree for a project, as a row or a tile"
    )]
    fn render_project(
        &self,
        index: usize,
        project: &Project,
        mark: SharedString,
        named: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let id = project.id;
        let selected = self.selected == Some(id);
        let tint = appearance::project_tint(id, cx);
        // How many of the project's chats need the user, or else how many
        // are open, which a working agent among them tints.
        let running: Vec<_> = project
            .chats
            .iter()
            .filter_map(|chat| self.running.get(&chat.id))
            .collect();
        let open = running.len();
        let needing = running
            .iter()
            .filter(|&&status| Tone::of(Some(*status)).needs_you())
            .count();
        let working = running
            .iter()
            .any(|status| matches!(status, Some(Status::Working)));
        let name = SharedString::from(project.name.clone());
        // The row already shows the name, so its hint tells where it is.
        let hint = if named {
            home_relative(&project.path)
        } else {
            name.to_string()
        };
        let hint = if open > 0 {
            format!("{hint}  ·  {}", summary(running.into_iter()))
        } else {
            hint
        };
        let hint = if index < NUMBERED {
            format!("{hint}  ·  Ctrl+{}", index + 1)
        } else {
            hint
        };
        let dragged = DraggedProject {
            id,
            index,
            mark: mark.clone(),
            name: name.clone(),
        };
        let drop_color = theme.drop_target;
        // While Ctrl is held, the mark gives way to the number that selects
        // the project, so nothing moves.
        let mark = if index < NUMBERED && self.held == Some(Held::Projects) {
            SharedString::from((index + 1).to_string())
        } else {
            mark
        };
        let count = || {
            if needing > 0 {
                badge(needing, true, false, cx)
            } else {
                badge(open, false, working, cx)
            }
        };
        let accent = palette.accent;
        let sidebar = cx.entity();

        let item = if named {
            h_flex()
                .id(("project", id.cast_unsigned()))
                .relative()
                .flex_none()
                .h(ROW_HEIGHT)
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .map(|this| {
                    if selected {
                        this.bg(theme.sidebar_accent)
                    } else {
                        this.hover(|style| style.bg(theme.sidebar_accent))
                    }
                })
                .child(project_mark(mark, tint, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .map(|this| {
                            if selected {
                                this.font_medium().text_color(theme.foreground)
                            } else {
                                this.text_color(palette.secondary_foreground)
                            }
                        })
                        .child(name),
                )
                .when(open > 0, |this| this.child(count()))
        } else {
            div()
                .id(("project", id.cast_unsigned()))
                .relative()
                .flex_none()
                .size(TILE_SIZE)
                .flex()
                .items_center()
                .justify_center()
                .group(TILE_GROUP)
                .rounded(px(10.))
                .bg(tint.soft)
                .text_color(tint.text)
                .text_sm()
                .font_medium()
                .font_family(theme.mono_font_family.clone())
                .child(mark)
                // A div paints its border over its children, so the border
                // is a child of its own, under the badge.
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(10.))
                        .border_1()
                        .map(|this| {
                            if selected {
                                this.border_color(palette.accent)
                            } else {
                                this.border_color(transparent_black())
                                    .group_hover(TILE_GROUP, |style| {
                                        style.border_color(palette.border_strong)
                                    })
                            }
                        }),
                )
                .when(open > 0, |this| {
                    this.child(count().absolute().top(px(-5.)).left(px(-5.)))
                })
        };

        item.cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            // Projects are reordered by dropping one on another.
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedProject>(move |style, dragged, _, _| {
                if dragged.id == id {
                    style
                } else if named {
                    drop_line(style, dragged.index < index, accent)
                } else {
                    style.bg(drop_color)
                }
            })
            .on_drop(cx.listener(move |_, dragged: &DraggedProject, _, cx| {
                if dragged.id != id {
                    cx.emit(NavEvent::MoveProject(dragged.id, id));
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
            .context_menu(move |menu, _, _| {
                let emit = |event| emit(&sidebar, event);
                menu.item(
                    PopupMenuItem::new("New chat…")
                        .icon(IconName::Plus)
                        .on_click(emit(NavEvent::NewChat(id))),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Archive")
                        .icon(ExtraIcon::Archive)
                        .on_click(emit(NavEvent::ArchiveProject(id))),
                )
                .item(
                    PopupMenuItem::new("Remove")
                        .icon(IconName::Delete)
                        .on_click(emit(NavEvent::RemoveProject(id))),
                )
            })
    }

    fn render_add_project(&self, named: bool, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let item = if named {
            h_flex()
                .id("add-project")
                .flex_none()
                .h(ROW_HEIGHT)
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .text_color(theme.muted_foreground)
                .hover(|style| style.bg(theme.sidebar_accent).text_color(theme.foreground))
                .child(
                    div()
                        .size(MARK_SIZE)
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(palette.border_strong)
                        .child(Icon::new(IconName::Plus).xsmall()),
                )
                .child("Add project")
        } else {
            div()
                .id("add-project")
                .flex_none()
                .size(TILE_SIZE)
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .border_1()
                .border_color(palette.border_strong)
                .text_color(theme.muted_foreground)
                .hover(|style| style.bg(theme.sidebar_accent))
                .child(Icon::new(IconName::Plus).small())
                .tooltip(|window, cx| Tooltip::new("Add a project").build(window, cx))
        };
        item.cursor_pointer()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::AddProject)))
    }
}
