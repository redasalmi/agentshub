//! The workspace's root view.

use super::*;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(reason) = self.runtime.read(cx).broken().cloned() {
            return render_broken(reason, cx);
        }
        let active = self
            .runtime
            .read(cx)
            .active()
            .filter(|id| self.panes.contains_key(id));
        let body = match (self.settings_shown, active) {
            (true, _) => Body::Settings(self.settings_view.clone()),
            (false, Some(chat)) => match self.shown_chat(chat, cx) {
                Some(shown) => Body::Chat(Box::new(shown)),
                None => Body::Empty(Empty.into_any_element()),
            },
            (false, None) => Body::Empty(
                self.render_empty_over_shell(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .child(self.render_empty(window, cx)),
                    window,
                    cx,
                ),
            ),
        };
        let parts = Parts {
            body,
            running_menu: self.running_menu.clone(),
        };
        let layout = self.layout.render(parts, window, cx);
        let theme = cx.theme();

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
            .child(layout)
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
