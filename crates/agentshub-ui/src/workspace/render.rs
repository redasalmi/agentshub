//! The workspace's root view.

use super::*;

impl Render for Workspace {
    #[expect(
        clippy::too_many_lines,
        reason = "the workspace's whole element tree, from one read of its state"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_width = window.viewport_size().width;
        let names = sidebar::names_width(window_width, cx);
        let room =
            window_width - clamp_sidebar_width(self.sidebar_width, window_width, names) - names;
        let settings_view = self.settings_view.clone();
        let settings = self.settings_shown.then(|| {
            settings_view
                .update(cx, |view, cx| view.render(room, window, cx))
                .into_any_element()
        });
        let theme = cx.theme();
        if let Some(reason) = self.runtime.read(cx).broken().cloned() {
            return render_broken(reason, cx);
        }
        let active = self
            .runtime
            .read(cx)
            .active()
            .filter(|id| self.panes.contains_key(id));

        h_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_shell))
            .on_action(cx.listener(Self::open_in_editor))
            .on_action(cx.listener(Self::open_search))
            .on_action(cx.listener(Self::new_chat_here))
            .on_action(cx.listener(Self::add_project_action))
            .on_action(cx.listener(Self::toggle_settings))
            .on_action(cx.listener(Self::toggle_history))
            .on_action(cx.listener(Self::rename_active))
            .on_action(cx.listener(Self::toggle_pin))
            .on_action(cx.listener(Self::delete_active))
            .on_action(cx.listener(Self::archive_active))
            .on_action(cx.listener(Self::archive_selected))
            .on_action(cx.listener(Self::show_archived))
            .on_action(cx.listener(Self::close_all_chats))
            .on_action(cx.listener(Self::close_idle_chats))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::toggle_project_names))
            .on_action(cx.listener(Self::use_theme))
            .on_action(cx.listener(Self::export_action))
            .on_action(cx.listener(Self::import_action))
            .on_action(cx.listener(Self::open_chat_at))
            .on_action(cx.listener(Self::select_project_at))
            .on_action(cx.listener(Self::show_running))
            .on_action(cx.listener(Self::next_needing_you))
            .on_action(cx.listener(Self::show_shortcuts))
            .on_key_down(cx.listener(Self::empty_key))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.modifiers_changed(event.modifiers, cx)
            }))
            // Seen before the terminal takes it: a key pressed with the
            // modifier is a shortcut, not a look at the numbers.
            .capture_key_down(
                cx.listener(|this, _: &KeyDownEvent, _, cx| this.release_modifiers(cx)),
            )
            .on_action(cx.listener(|this, _: &terminal_view::ZoomIn, window, cx| {
                this.zoom_terminals(1., window, cx)
            }))
            .on_action(cx.listener(|this, _: &terminal_view::ZoomOut, window, cx| {
                this.zoom_terminals(-1., window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &terminal_view::ResetZoom, window, cx| {
                    let reset =
                        DEFAULT_TERMINAL_FONT_SIZE - cx.global::<Settings>().terminal_font_size;
                    this.zoom_terminals(reset, window, cx)
                }),
            )
            // Follows the pointer anywhere in the window while the sidebar's
            // edge is dragged, since it leaves the edge's thin band at once.
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeSidebar>, window, cx| {
                    let window_width = window.viewport_size().width;
                    let names = sidebar::names_width(window_width, cx);
                    let width = clamp_sidebar_width(
                        event.event.position.x - event.bounds.left() - names,
                        window_width,
                        names,
                    );
                    if width != this.sidebar_width {
                        this.sidebar_width = width;
                        cx.notify();
                    }
                }),
            )
            // The rail's edge likewise. The rail starts where the window
            // does.
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeRail>, window, cx| {
                    let width = event.event.position.x - event.bounds.left();
                    let rail_width = cx.global::<Settings>().rail_width;
                    if width < sidebar::FOLD_RAIL_BELOW {
                        // Folded, it keeps the width to unfold to.
                        this.resize_rail(false, rail_width, cx);
                    } else {
                        let max = rail_room(window.viewport_size().width)
                            .min(sidebar::MAX_RAIL_WIDTH)
                            .max(sidebar::MIN_NAMED_RAIL_WIDTH);
                        let width = width.clamp(sidebar::MIN_NAMED_RAIL_WIDTH, max);
                        this.resize_rail(true, f32::from(width), cx);
                    }
                }),
            )
            .child(
                div()
                    .relative()
                    .w(clamp_sidebar_width(self.sidebar_width, window_width, names) + names)
                    .h_full()
                    .flex_none()
                    // Terminal output re-renders the window many times a
                    // second; the sidebar and the history only need to render
                    // again when their own state changes.
                    .child(
                        self.sidebar
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    )
                    .child(self.render_rail_edge(window_width, cx))
                    .child(self.render_sidebar_edge(cx)),
            )
            // The sheet the chat is drawn on, set on the sidebar's ground
            // under the toolbar.
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_toolbar(room, window, cx))
                    .child(
                        v_flex().flex_1().min_h_0().px_2().pb_2().child(
                            v_flex()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .bg(theme.background)
                                .border_1()
                                .border_color(theme.border)
                                .rounded(theme.radius_lg)
                                .shadow_xs()
                                .child(v_flex().flex_1().min_h_0().map(|this| {
                                    match (settings, active) {
                                        (Some(settings), _) => this.child(settings),
                                        (None, Some(chat)) => {
                                            this.child(self.render_chat(chat, window, cx))
                                        }
                                        (None, None) => this.child(
                                            self.render_empty_over_shell(
                                                div()
                                                    .size_full()
                                                    .overflow_hidden()
                                                    .child(self.render_empty(window, cx)),
                                                window,
                                                cx,
                                            ),
                                        ),
                                    }
                                }))
                                .child(self.render_footer(room, window, cx)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn render_broken(reason: SharedString, cx: &Context<Workspace>) -> AnyElement {
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

/// The projects of a database about to be imported, each with its first few
/// chats.
pub(super) fn render_import(projects: &[store::Project], theme: &Theme) -> impl IntoElement {
    /// The chats listed under each project; the others are counted.
    const SHOWN_CHATS: usize = 3;
    v_flex()
        .id("import-projects")
        .max_h(px(260.))
        .overflow_y_scroll()
        .gap_3()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .children(projects.iter().map(|project| {
            let more = project.chats.len().saturating_sub(SHOWN_CHATS);
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_medium()
                                .text_color(theme.foreground)
                                .child(project.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(home_relative(&project.path)),
                        ),
                )
                .children(project.chats.iter().take(SHOWN_CHATS).map(|chat| {
                    div()
                        .pl_3()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(chat.title.clone().unwrap_or_else(|| "New chat".into()))
                }))
                .when(more > 0, |list| {
                    list.child(
                        div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child(format!("and {} more", plural(more, "chat"))),
                    )
                })
                .when(project.chats.is_empty(), |list| {
                    list.child(
                        div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child("No chats"),
                    )
                })
        }))
}
