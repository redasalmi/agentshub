//! The sessions a chat went through before the one its CLI is in, shown
//! read-only above its terminal from the copy the database keeps.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::sidebar::age;
use crate::store::{Item, Session, Store};
use crate::transcript::Kind;

mod render;

use render::ended;

const ROW_HEIGHT: Pixels = px(32.);

/// How much of a tool's input shows beside its name.
const INPUT_CHARS: usize = 200;

/// How many of an expanded session's items show at a time. A long session has
/// thousands, and the list isn't virtualised, so the rest wait behind a
/// button.
const ITEMS_PAGE: usize = 100;

pub struct History {
    store: Store,
    chat_id: i64,
    sessions: Vec<Earlier>,
    /// Whether the list shows. The chat's header has the button that opens
    /// it.
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
    /// The expanded sessions to reload once the sessions are listed, and the
    /// listing under way, which a newer one replaces.
    reload: HashSet<i64>,
    _listing: Option<Task<()>>,
}

struct Earlier {
    id: i64,
    title: SharedString,
    started_at: i64,
    ended: SharedString,
}

impl History {
    pub fn new(store: Store, chat_id: i64, open: bool) -> Self {
        Self {
            store,
            chat_id,
            sessions: Vec::new(),
            open,
            expanded: HashMap::new(),
            limits: HashMap::new(),
            more: HashSet::new(),
            errors: HashMap::new(),
            shown: HashSet::new(),
            reload: HashSet::new(),
            _listing: None,
        }
    }

    /// Lists the chat's sessions again, but for `current`, the one its CLI is
    /// in, and reloads the expanded ones whose transcript read, in `changed`,
    /// found something.
    pub fn refresh(&mut self, current: Option<i64>, changed: &[i64], cx: &mut Context<Self>) {
        self.reload.extend(changed);
        let sessions = self.store.sessions(self.chat_id);
        self._listing = Some(cx.spawn(async move |this, cx| {
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
        self.expanded
            .retain(|id, _| self.sessions.iter().any(|session| session.id == *id));
        self.limits.retain(|id, _| self.expanded.contains_key(id));
        self.more.retain(|id| self.expanded.contains_key(id));
        self.errors.retain(|id, _| self.expanded.contains_key(id));
        for id in std::mem::take(&mut self.reload) {
            if self.expanded.contains_key(&id) {
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
        let store = self.store.clone();
        let limit = self.limits.get(&id).copied().unwrap_or(ITEMS_PAGE);
        self.expanded.entry(id).or_default();
        cx.spawn(async move |this, cx| {
            let read = store.read_transcript(id).await;
            let items = store.items(id, limit).await;
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
