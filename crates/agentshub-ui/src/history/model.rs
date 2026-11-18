//! The history's data: the sessions, what is expanded and what is loading.

use std::rc::Rc;

use agentshub_core::store::HistorySource;

use super::*;

pub struct HistoryModel {
    pub(super) source: Rc<dyn HistorySource>,
    pub(super) chat_id: i64,
    pub(super) sessions: Vec<Earlier>,
    /// The items of the sessions that are expanded, once they're loaded.
    pub(super) expanded: HashMap<i64, Option<Vec<Item>>>,
    /// How many items of each expanded session show.
    pub(super) limits: HashMap<i64, usize>,
    /// The sessions that may have more items before the ones loaded.
    pub(super) more: HashSet<i64>,
    /// What failed to load, for the sessions that couldn't be shown.
    pub(super) errors: HashMap<i64, SharedString>,
    /// The reasoning and tool outputs that are expanded, by item.
    pub(super) shown: HashSet<i64>,
    /// The expanded sessions to reload once the sessions are listed, and the
    /// listing under way, which a newer one replaces.
    pub(super) reload: HashSet<i64>,
    pub(super) listing: Option<Task<()>>,
}

impl HistoryModel {
    pub(super) fn new(source: Rc<dyn HistorySource>, chat_id: i64) -> Self {
        Self {
            source,
            chat_id,
            sessions: Vec::new(),
            expanded: HashMap::new(),
            limits: HashMap::new(),
            more: HashSet::new(),
            errors: HashMap::new(),
            shown: HashSet::new(),
            reload: HashSet::new(),
            listing: None,
        }
    }
}

/// Lists a chat's sessions.
pub(super) async fn sessions(
    source: Rc<dyn HistorySource>,
    chat_id: i64,
) -> anyhow::Result<Vec<Session>> {
    let (send, recv) = smol::channel::bounded(1);
    source.sessions(
        chat_id,
        Box::new(move |sessions| {
            send.try_send(sessions).ok();
        }),
    );
    recv.recv().await.map_err(|err| anyhow::anyhow!("{err}"))?
}

/// Reads a session's transcript.
pub(super) async fn read_transcript(
    source: Rc<dyn HistorySource>,
    id: i64,
) -> anyhow::Result<bool> {
    let (send, recv) = smol::channel::bounded(1);
    source.read_transcript(
        id,
        Box::new(move |read| {
            send.try_send(read).ok();
        }),
    );
    recv.recv().await.map_err(|err| anyhow::anyhow!("{err}"))?
}

/// Loads a session's last page of items.
pub(super) async fn items(
    source: Rc<dyn HistorySource>,
    id: i64,
    limit: usize,
) -> anyhow::Result<Vec<Item>> {
    let (send, recv) = smol::channel::bounded(1);
    source.items(
        id,
        limit,
        Box::new(move |items| {
            send.try_send(items).ok();
        }),
    );
    recv.recv().await.map_err(|err| anyhow::anyhow!("{err}"))?
}
