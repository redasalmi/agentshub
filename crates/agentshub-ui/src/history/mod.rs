//! The sessions a chat went through before the one its CLI is in, shown
//! read-only above its terminal from the copy the database keeps.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use agentshub_core::store::{HistorySource, Item, Session};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use agentshub_core::format::age;
use agentshub_core::transcript::Kind;

mod render;

use model::{HistoryModel, items as read_items, read_transcript, sessions as list_sessions};
use render::ended;

const ROW_HEIGHT: Pixels = px(32.);

/// How much of a tool's input shows beside its name.
const INPUT_CHARS: usize = 200;

/// How many of an expanded session's items show at a time. A long session has
/// thousands, and the list isn't virtualised, so the rest wait behind a
/// button.
const ITEMS_PAGE: usize = 100;

mod model;

use std::ops::{Deref, DerefMut};

pub struct History {
    model: HistoryModel,
    /// Whether the list shows. The chat's header has the button that opens
    /// it.
    open: bool,
}

impl Deref for History {
    type Target = HistoryModel;

    fn deref(&self) -> &Self::Target {
        &self.model
    }
}

impl DerefMut for History {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.model
    }
}

struct Earlier {
    id: i64,
    title: SharedString,
    started_at: i64,
    ended: SharedString,
}

impl History {
    pub fn new(source: Rc<dyn HistorySource>, chat_id: i64, open: bool) -> Self {
        Self {
            model: HistoryModel::new(source, chat_id),
            open,
        }
    }

    /// Lists the chat's sessions again, but for `current`, the one its CLI is
    /// in, and reloads the expanded ones whose transcript read, in `changed`,
    /// found something.
    pub fn refresh(&mut self, current: Option<i64>, changed: &[i64], cx: &mut Context<Self>) {
        self.reload.extend(changed);
        let sessions = list_sessions(self.source.clone(), self.chat_id);
        self.listing = Some(cx.spawn(async move |this, cx| {
            let sessions = sessions.await;
            this.update(cx, |this, cx| this.list(sessions, current, cx))
                .ok();
        }));
    }

    fn list(
        &mut self,
        sessions: anyhow::Result<Vec<Session>>,
        current: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let sessions = match sessions {
            Ok(sessions) => sessions,
            Err(err) => {
                eprintln!("can't list the sessions of chat {}: {err}", self.chat_id);
                return;
            }
        };
        self.sessions = sessions
            .iter()
            .filter(|session| session.prompted && Some(session.id) != current)
            .map(|session| Earlier {
                id: session.id,
                title: session.title.clone().unwrap_or_default().into(),
                started_at: session.started_at,
                ended: ended(session).into(),
            })
            .collect();
        {
            let sessions = &self.model.sessions;
            self.model
                .expanded
                .retain(|id, _| sessions.iter().any(|session| session.id == *id));
        }
        let expanded = &self.model.expanded;
        self.model.limits.retain(|id, _| expanded.contains_key(id));
        self.model.more.retain(|id| expanded.contains_key(id));
        self.model.errors.retain(|id, _| expanded.contains_key(id));
        for id in std::mem::take(&mut self.model.reload) {
            if self.model.expanded.contains_key(&id) {
                self.load(id, cx);
            }
        }
        cx.notify();
    }

    /// How many earlier sessions there are.
    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        cx.notify();
    }

    /// How tall the list is, at most `max`, or `None` while it's closed or
    /// empty.
    pub fn height(&self, max: Pixels) -> Option<Pixels> {
        if self.sessions.is_empty() || !self.open {
            return None;
        }
        if !self.expanded.is_empty() {
            return Some(max);
        }
        // The border under it takes a pixel.
        Some((ROW_HEIGHT * self.sessions.len() as f32 + px(1.)).min(max))
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
        let source = self.source.clone();
        let limit = self.limits.get(&id).copied().unwrap_or(ITEMS_PAGE);
        self.expanded.entry(id).or_default();
        cx.spawn(async move |this, cx| {
            let read = read_transcript(source.clone(), id).await;
            let items = read_items(source, id, limit).await;
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
}
