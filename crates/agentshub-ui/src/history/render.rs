//! Rendering the earlier sessions and their items.

use super::*;

impl History {
    pub(super) fn render_session(&self, session: &Earlier, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = session.id;
        let items = self.expanded.get(&id);

        v_flex()
            .child(
                h_flex()
                    .id(("session", id as u64))
                    .h(ROW_HEIGHT)
                    .flex_none()
                    .px_6()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .child(chevron(items.is_some(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(session.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", session.ended, age(session.started_at))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_session(id, cx))),
            )
            .children(items.map(|items| {
                v_flex()
                    .pl_12()
                    .pr_6()
                    .pt_1()
                    .pb_4()
                    .gap_3()
                    .map(|this| match items {
                        None => this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Loading…"),
                        ),
                        Some(items) => this
                            .when(self.more.contains(&id), |this| {
                                this.child(
                                    h_flex()
                                        .id(("earlier", id as u64))
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .hover(|style| style.text_color(theme.foreground))
                                        .child(format!("Show {ITEMS_PAGE} earlier items"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.show_more(id, cx)
                                        })),
                                )
                            })
                            .children(items.iter().map(|item| self.render_item(item, cx))),
                    })
                    .when_some(self.errors.get(&id).cloned(), |this, error| {
                        this.child(div().text_sm().text_color(theme.danger).child(error))
                    })
            }))
    }

    fn render_item(&self, item: &Item, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let payload = &item.payload;
        let text = || SharedString::from(payload["text"].as_str().unwrap_or_default().to_string());
        let shown = self.shown.contains(&item.id);
        let id = item.id;
        let toggle = |label: SharedString| {
            h_flex()
                .id(("item", id as u64))
                .gap_1p5()
                .min_w_0()
                .cursor_pointer()
                .text_xs()
                .text_color(theme.muted_foreground)
                .hover(|style| style.text_color(theme.foreground))
                .child(chevron(shown, cx))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_item(id, cx)))
        };

        match item.kind {
            Kind::User => div()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.secondary)
                .text_sm()
                .child(text())
                .into_any_element(),
            Kind::Assistant => div().text_sm().child(text()).into_any_element(),
            Kind::Reasoning => v_flex()
                .gap_1()
                .child(toggle("Thinking".into()))
                .when(shown, |this| {
                    this.child(
                        div()
                            .pl_5()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(text()),
                    )
                })
                .into_any_element(),
            Kind::Tool => {
                let name = payload["name"].as_str().unwrap_or("tool").to_string();
                let input = match &payload["input"] {
                    Value::String(input) => input.clone(),
                    input => input.to_string(),
                };
                let summary: String = input.chars().take(INPUT_CHARS).collect();
                let output = payload["output"]
                    .as_str()
                    .filter(|output| !output.is_empty())
                    .map(str::to_string);
                v_flex()
                    .gap_1()
                    .child(
                        toggle(name.into())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(summary),
                            )
                            .when(payload["error"] == true, |this| {
                                this.child(div().text_color(theme.danger).child("failed"))
                            }),
                    )
                    .when(shown, |this| {
                        this.child(
                            v_flex()
                                .ml_5()
                                .p_2()
                                .gap_2()
                                .rounded(theme.radius)
                                .bg(theme.background)
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .child(input)
                                .child(
                                    div()
                                        .pt_2()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .text_color(theme.muted_foreground)
                                        .child(output.unwrap_or_else(|| "No output".into())),
                                ),
                        )
                    })
                    .into_any_element()
            }
            Kind::Compaction => h_flex()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().h_px().bg(theme.border))
                .child("Conversation compacted")
                .child(div().flex_1().h_px().bg(theme.border))
                .into_any_element(),
            Kind::Notice => div()
                .text_sm()
                .italic()
                .text_color(theme.muted_foreground)
                .child(text())
                .into_any_element(),
        }
    }
}

impl Render for History {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("sessions")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.muted)
            .border_b_1()
            .border_color(theme.border)
            .children(
                self.sessions
                    .iter()
                    .map(|session| self.render_session(session, cx)),
            )
    }
}

fn chevron(expanded: bool, cx: &App) -> Icon {
    Icon::new(if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .xsmall()
    .text_color(cx.theme().muted_foreground)
}

/// How a session ended, from why the chat left it for another one.
pub(super) fn ended(session: &Session) -> String {
    let ended = match session.ended.as_deref() {
        Some("clear") => "Cleared",
        Some("new") => "Replaced by /new",
        Some("resume") => "Replaced by /resume",
        Some("fork") => "Forked",
        _ => "Ended",
    };
    if session.missing {
        format!("{ended}, transcript gone")
    } else {
        ended.to_string()
    }
}
