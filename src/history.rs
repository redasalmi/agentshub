//! The sessions a chat went through before the one its CLI is in, shown
//! read-only above its terminal from the copy the database keeps.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::db::{Db, Item, Session};
use crate::sidebar::age;
use crate::transcript::Kind;

const HEADER_HEIGHT: Pixels = px(32.);
const ROW_HEIGHT: Pixels = px(32.);

/// How much of a tool's input shows beside its name.
const INPUT_CHARS: usize = 200;

/// How many of an expanded session's items show at a time. A long session has
/// thousands, and the list isn't virtualised, so the rest wait behind a
/// button.
const ITEMS_PAGE: usize = 100;

pub struct History {
    db: Db,
    chat_id: i64,
    sessions: Vec<Earlier>,
    /// Whether the list shows, rather than only the line that sums it up.
    open: bool,
    /// The items of the sessions that are expanded, once they're loaded.
    expanded: HashMap<i64, Option<Vec<Item>>>,
    /// How many items of each expanded session show.
    limits: HashMap<i64, usize>,
    /// The sessions that may have more items before the ones loaded.
    more: HashSet<i64>,
    /// What failed to load, for the sessions that couldn't be shown.
    errors: HashMap<i64, SharedString>,
    /// The reasoning and tool outputs that are expanded, by item.
    shown: HashSet<i64>,
}

struct Earlier {
    id: i64,
    title: SharedString,
    started_at: i64,
    ended: SharedString,
}

impl History {
    pub fn new(db: Db, chat_id: i64) -> Self {
        Self {
            db,
            chat_id,
            sessions: Vec::new(),
            open: true,
            expanded: HashMap::new(),
            limits: HashMap::new(),
            more: HashSet::new(),
            errors: HashMap::new(),
            shown: HashSet::new(),
        }
    }

    /// Lists the chat's sessions again, but for `current`, the one its CLI is
    /// in, and reloads the expanded ones whose transcript read, in `changed`,
    /// found something.
    pub fn refresh(&mut self, current: Option<i64>, changed: &[i64], cx: &mut Context<Self>) {
        let sessions = match self.db.sessions(self.chat_id) {
            Ok(sessions) => sessions,
            Err(err) => {
                eprintln!("can't list the sessions of chat {}: {err}", self.chat_id);
                return;
            }
        };
        self.sessions = sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.prompted && Some(session.id) != current)
            .map(|(index, session)| Earlier {
                id: session.id,
                title: session.title.clone().unwrap_or_default().into(),
                started_at: session.started_at,
                ended: ended(session, sessions.get(index + 1)).into(),
            })
            .collect();
        self.expanded
            .retain(|id, _| self.sessions.iter().any(|session| session.id == *id));
        self.limits.retain(|id, _| self.expanded.contains_key(id));
        self.more.retain(|id| self.expanded.contains_key(id));
        self.errors.retain(|id, _| self.expanded.contains_key(id));
        for id in changed {
            if self.expanded.contains_key(id) {
                self.load(*id, cx);
            }
        }
        cx.notify();
    }

    /// How tall the list is, at most `max`, or `None` while it's empty.
    pub fn height(&self, max: Pixels) -> Option<Pixels> {
        if self.sessions.is_empty() {
            return None;
        }
        // The border under it takes a pixel.
        let rows = if !self.open {
            0.
        } else if self.expanded.is_empty() {
            self.sessions.len() as f32
        } else {
            return Some(max);
        };
        Some((HEADER_HEIGHT + ROW_HEIGHT * rows + px(1.)).min(max))
    }

    fn toggle_session(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.expanded.remove(&id).is_none() {
            self.load(id, cx);
        }
        cx.notify();
    }

    /// Reads the session's transcript, then loads its last page. Expanding a
    /// session is the first time its transcript has to be read since the chat
    /// opened, so the read and the load happen together.
    fn load(&mut self, id: i64, cx: &mut Context<Self>) {
        let db = self.db.clone();
        let limit = self.limits.get(&id).copied().unwrap_or(ITEMS_PAGE);
        self.expanded.entry(id).or_default();
        cx.spawn(async move |this, cx| {
            let (read, items) = cx
                .background_executor()
                .spawn(async move {
                    let read = crate::transcript::read(&db, id);
                    (read, db.items(id, limit))
                })
                .await;
            this.update(cx, |this, cx| {
                if !this.expanded.contains_key(&id) {
                    return;
                }
                match read {
                    Ok(_) => {
                        this.errors.remove(&id);
                    }
                    Err(err) => {
                        this.errors
                            .insert(id, format!("Couldn't read this transcript: {err}").into());
                    }
                }
                match items {
                    Ok(items) => {
                        if items.len() >= limit {
                            this.more.insert(id);
                        } else {
                            this.more.remove(&id);
                        }
                        this.expanded.insert(id, Some(items));
                    }
                    Err(err) => {
                        eprintln!("can't load the items of session {id}: {err}");
                        this.errors
                            .insert(id, format!("Couldn't load this session: {err}").into());
                        this.expanded.insert(id, Some(Vec::new()));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_item(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.shown.remove(&id) {
            self.shown.insert(id);
        }
        cx.notify();
    }

    fn show_more(&mut self, session: i64, cx: &mut Context<Self>) {
        *self.limits.entry(session).or_insert(ITEMS_PAGE) += ITEMS_PAGE;
        self.load(session, cx);
        cx.notify();
    }

    fn render_session(&self, session: &Earlier, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = session.id;
        let items = self.expanded.get(&id);

        v_flex()
            .child(
                h_flex()
                    .id(("session", id as u64))
                    .h(ROW_HEIGHT)
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.muted))
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
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", session.ended, age(session.started_at))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_session(id, cx))),
            )
            .children(items.map(|items| {
                v_flex()
                    .pl_8()
                    .pr_4()
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
                                .bg(theme.muted)
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
        let summary = match self.sessions.len() {
            1 => "1 earlier session".to_string(),
            count => format!("{count} earlier sessions"),
        };

        v_flex()
            .size_full()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .id("earlier-sessions")
                    .h(HEADER_HEIGHT)
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .cursor_pointer()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.text_color(theme.foreground))
                    .child(chevron(self.open, cx))
                    .child(summary)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open = !this.open;
                        cx.notify();
                    })),
            )
            .when(self.open, |this| {
                this.child(
                    v_flex()
                        .id("sessions")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(
                            self.sessions
                                .iter()
                                .map(|session| self.render_session(session, cx)),
                        ),
                )
            })
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

/// How a session ended, from why the chat left it for another one. Rows from
/// before the database kept that say nothing, so the next session's reason
/// stands in, as it always did.
fn ended(session: &Session, next: Option<&Session>) -> String {
    let reason = session
        .ended
        .as_deref()
        .or_else(|| next.and_then(|next| next.reason.as_deref()));
    let ended = match reason {
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
