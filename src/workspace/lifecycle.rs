//! Where each chat is on its way in or out, and whether it is being deleted,
//! kept apart from the window so every order of events can be tested.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::worktree::Worktree;

/// Closes once a stopped CLI, and everything it started, has been reaped.
pub type Reaped = smol::channel::Receiver<()>;

/// A chat on its way in or out. One that has no phase is open, if the
/// workspace has it, or closed.
enum Phase {
    /// Its last session is being read before its CLI starts, with whether it
    /// still should once that is done. Closing all the chats cancels it, and
    /// opening it again revives it.
    Opening { start: bool },
    /// Its CLI was stopped to free what it held, and isn't reaped yet. Until
    /// it is, the CLI may still write to `session`, so nothing else may run
    /// in it, and the chat doesn't start again.
    Stopping {
        reaped: Reaped,
        session: Option<i64>,
    },
}

/// A chat being deleted, with what goes once that is saved.
struct Deleting {
    /// The worktree Agents Hub made for it, and its project's folder.
    worktree: Option<(Worktree, PathBuf)>,
    /// What says its stopped CLI has been reaped.
    reaped: Option<Reaped>,
    /// How many deletes of it, its own or its project's, aren't saved yet.
    pending: usize,
}

/// A chat whose delete was saved, with what to remove once its CLI is gone.
pub struct Deleted {
    pub worktree: Option<(Worktree, PathBuf)>,
    pub reaped: Option<Reaped>,
}

/// A chat being added, until that is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Saving(u64);

/// A chat being added, with the worktree made for it.
struct Adding {
    worktree: Option<(Worktree, PathBuf)>,
    /// How many times a project had been archived when it was asked for.
    archived_projects: u64,
}

/// What the workspace sees of a chat once adding it is saved.
pub struct Saved {
    /// Its id, or none if its project was removed before it was added.
    pub id: Result<Option<i64>, String>,
    /// Whether it was still there once added.
    pub kept: Result<bool, String>,
    /// How many times all the chats had been closed when it was asked for.
    pub closed_all: u64,
    pub project_removing: bool,
    pub project_archived: bool,
    pub chat_archived: bool,
    pub chat_open: bool,
    /// Whether there is a window to start it in.
    pub window: bool,
}

/// What to do with a chat once adding it is saved.
#[derive(Debug, PartialEq)]
pub enum Added {
    /// It wasn't added, or is gone already, so its worktree goes.
    Lost,
    /// It couldn't be added, so its worktree goes.
    Failed(String),
    /// Its project is being removed, so it goes too, with its worktree.
    Delete(i64),
    /// Whether it's still there is unknown, so it is left as it is.
    Unknown(String),
    /// It's listed, but doesn't start.
    Listed,
    Start(i64),
}

#[derive(Default)]
pub struct Lifecycle {
    phases: HashMap<i64, Phase>,
    deleting: HashMap<i64, Deleting>,
    adding: HashMap<Saving, Adding>,
    next_saving: u64,
    /// How many times all the chats were closed. A new chat asked for before
    /// then doesn't start.
    closed_all: u64,
    /// How many times a project was archived, which a chat added meanwhile
    /// doesn't start in.
    archived_projects: u64,
}

impl Lifecycle {
    /// What says chat `id`'s stopped CLI has been reaped, while it hasn't
    /// yet: the chat opens again once it is.
    pub fn reaping(&self, id: i64) -> Option<Reaped> {
        match self.phases.get(&id) {
            Some(Phase::Stopping { reaped, .. }) if !reaped.is_closed() => Some(reaped.clone()),
            _ => None,
        }
    }

    /// Whether chat `id`'s last session is being read before its CLI starts.
    pub fn is_opening(&self, id: i64) -> bool {
        matches!(self.phases.get(&id), Some(Phase::Opening { .. }))
    }

    /// Starts opening chat `id`, which has no CLI, unless its stopped CLI
    /// isn't reaped yet (see `reaping`). Returns whether its last session
    /// has to be read; a chat already opening starts once that read is done,
    /// even if it was cancelled.
    pub fn open(&mut self, id: i64) -> bool {
        if let Some(Phase::Opening { start }) = self.phases.get_mut(&id) {
            *start = true;
            return false;
        }
        self.phases.insert(id, Phase::Opening { start: true });
        true
    }

    /// Ends opening chat `id`, once its last session is read, and says
    /// whether its CLI should start.
    pub fn opened(&mut self, id: i64) -> bool {
        match self.phases.get(&id) {
            Some(Phase::Opening { start }) => {
                let start = *start;
                self.phases.remove(&id);
                start
            }
            _ => false,
        }
    }

    /// Keeps chat `id` from starting once its last session is read.
    pub fn cancel_opening(&mut self, id: i64) {
        if let Some(Phase::Opening { start }) = self.phases.get_mut(&id) {
            *start = false;
        }
    }

    /// Closing all the chats keeps every chat being opened, or asked for
    /// before, from starting.
    pub fn close_all(&mut self) {
        self.closed_all += 1;
        for phase in self.phases.values_mut() {
            if let Phase::Opening { start } = phase {
                *start = false;
            }
        }
    }

    /// The chats being opened.
    pub fn opening(&self) -> impl Iterator<Item = i64> + '_ {
        self.phases.iter().filter_map(|(&id, phase)| match phase {
            Phase::Opening { .. } => Some(id),
            Phase::Stopping { .. } => None,
        })
    }

    /// Chat `id`'s CLI was stopped in `session`, and `reaped` closes once
    /// it's gone.
    pub fn stop(&mut self, id: i64, reaped: Reaped, session: Option<i64>) {
        self.phases.insert(id, Phase::Stopping { reaped, session });
    }

    /// Chat `id`'s CLI stopped by `reaped`'s stop was in `session` after all.
    pub fn stopped_in(&mut self, id: i64, reaped: &Reaped, session: Option<i64>) {
        if let Some(Phase::Stopping {
            reaped: stopping,
            session: stopped_in,
        }) = self.phases.get_mut(&id)
            && stopping.same_channel(reaped)
        {
            *stopped_in = session;
        }
    }

    /// Chat `id`'s CLI stopped by `reaped`'s stop is reaped. The chat may
    /// have been opened and stopped again since.
    pub fn reaped(&mut self, id: i64, reaped: &Reaped) {
        if let Some(Phase::Stopping {
            reaped: stopping, ..
        }) = self.phases.get(&id)
            && stopping.same_channel(reaped)
        {
            self.phases.remove(&id);
        }
    }

    /// Whether a stopped CLI other than chat `id`'s may still write to
    /// `session`.
    pub fn stopped_elsewhere(&self, id: i64, session: i64) -> bool {
        self.stopping().any(|(each, reaped, stopped)| {
            each != id && stopped == Some(session) && !reaped.is_closed()
        })
    }

    /// The chats whose CLI was stopped, with what says it's reaped and the
    /// session it was in.
    pub fn stopping(&self) -> impl Iterator<Item = (i64, &Reaped, Option<i64>)> + '_ {
        self.phases.iter().filter_map(|(&id, phase)| match phase {
            Phase::Stopping { reaped, session } => Some((id, reaped, *session)),
            Phase::Opening { .. } => None,
        })
    }

    /// Keeps chat `id` from being opened until deleting it is saved. A
    /// chat can be deleted again meanwhile, with its project, once its CLI
    /// is already stopped, so what says that CLI was reaped is kept.
    pub fn mark_deleting(
        &mut self,
        id: i64,
        worktree: Option<(Worktree, PathBuf)>,
        reaped: Option<Reaped>,
    ) {
        let deleting = self.deleting.entry(id).or_insert(Deleting {
            worktree,
            reaped: None,
            pending: 0,
        });
        deleting.pending += 1;
        if reaped.is_some() {
            deleting.reaped = reaped;
        }
    }

    /// Lets chat `id` be opened again once deleting it is done, and gives
    /// what to remove if it was deleted.
    pub fn finish_deleting(&mut self, id: i64, deleted: bool) -> Option<Deleted> {
        let deleting = self.deleting.get_mut(&id)?;
        deleting.pending -= 1;
        // Another delete of it may still be saved.
        if !deleted && deleting.pending > 0 {
            return None;
        }
        let Deleting {
            worktree, reaped, ..
        } = self.deleting.remove(&id)?;
        deleted.then_some(Deleted { worktree, reaped })
    }

    pub fn is_deleting(&self, id: i64) -> bool {
        self.deleting.contains_key(&id)
    }

    pub fn closed_all(&self) -> u64 {
        self.closed_all
    }

    /// A project was archived, which stops its chats.
    pub fn archive_project(&mut self) {
        self.archived_projects += 1;
    }

    /// Starts adding a chat, made the `worktree` of its project if it has
    /// one, until `added` says what then becomes of it.
    pub fn save(&mut self, worktree: Option<(Worktree, PathBuf)>) -> Saving {
        let saving = Saving(self.next_saving);
        self.next_saving += 1;
        self.adding.insert(
            saving,
            Adding {
                worktree,
                archived_projects: self.archived_projects,
            },
        );
        saving
    }

    /// Ends adding a chat, saying what becomes of it, and gives its worktree
    /// back.
    pub fn added(&mut self, saving: Saving, saved: Saved) -> (Added, Option<(Worktree, PathBuf)>) {
        let Adding {
            worktree,
            archived_projects,
        } = self.adding.remove(&saving).expect("a chat is added once");
        let added = match saved.id {
            Ok(Some(id)) => self.decide(id, archived_projects, saved),
            Ok(None) => Added::Lost,
            Err(err) => Added::Failed(err),
        };
        (added, worktree)
    }

    fn decide(&self, id: i64, archived_projects: u64, saved: Saved) -> Added {
        // Its project may have been removed while it was added, with the
        // chats it had before this one.
        if saved.project_removing {
            return Added::Delete(id);
        }
        match saved.kept {
            Ok(true) => {}
            Ok(false) => return Added::Lost,
            Err(err) => return Added::Unknown(err),
        }
        // All the chats were closed while it was added, this one with them,
        // or it was opened, archived or deleted from a listing since it was
        // saved, or its project was archived, which stops its agents.
        if self.closed_all != saved.closed_all
            || saved.chat_archived
            || self.is_deleting(id)
            || self.is_opening(id)
            || saved.chat_open
            || self.archived_projects != archived_projects && saved.project_archived
            || !saved.window
        {
            return Added::Listed;
        }
        Added::Start(id)
    }

    /// The worktrees of chats still being added, which shutdown removes
    /// unless a chat took them.
    pub fn take_adding(&mut self) -> Vec<(Worktree, PathBuf)> {
        std::mem::take(&mut self.adding)
            .into_values()
            .filter_map(|adding| adding.worktree)
            .collect()
    }

    /// The chats being deleted, which shutdown finishes.
    pub fn take_deleting(&mut self) -> Vec<(i64, Deleted)> {
        std::mem::take(&mut self.deleting)
            .into_iter()
            .map(|(id, deleting)| {
                (
                    id,
                    Deleted {
                        worktree: deleting.worktree,
                        reaped: deleting.reaped,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{Lifecycle, Reaped};

    const CHATS: [i64; 2] = [1, 2];

    /// What happens to the chats, from the user or from work that finishes.
    #[derive(Clone, Copy, Debug)]
    enum Event {
        /// The user opens the chat.
        Open(i64),
        /// The read of its last session finishes, with or without a window.
        ReadDone(i64),
        ReadWithoutWindow(i64),
        /// The user closes it, or archives it, which also cancels its opening.
        Close(i64),
        Archive(i64),
        CloseAll,
        /// Its stopped CLI is reaped, and then the task waiting for that runs.
        Reap(i64),
        ReapedTask(i64),
        /// The user deletes it, and that is saved, or fails to be.
        Delete(i64),
        Saved(i64, bool),
    }

    struct Stop {
        id: i64,
        reaped: Reaped,
        sender: Option<smol::channel::Sender<()>>,
        /// Whether a task waits for it, as for a close; a delete's doesn't.
        task: bool,
    }

    /// The workspace around the lifecycle, and what it should hold.
    #[derive(Default)]
    struct World {
        lifecycle: Lifecycle,
        open: HashSet<i64>,
        /// Reads under way, with whether their chat should start.
        reads: HashMap<i64, bool>,
        stops: Vec<Stop>,
        /// What the stopping chats wait for.
        stopping: HashMap<i64, Reaped>,
        /// Deletes not saved yet.
        deletes: HashMap<i64, usize>,
        /// Whether a saved delete still has to give what to remove.
        to_remove: HashSet<i64>,
        gone: HashSet<i64>,
    }

    impl World {
        fn stop(&mut self, id: i64, task: bool) -> Reaped {
            let (sender, reaped) = smol::channel::bounded(1);
            self.stops.push(Stop {
                id,
                reaped: reaped.clone(),
                sender: Some(sender),
                task,
            });
            reaped
        }

        fn close(&mut self, id: i64) {
            if self.open.remove(&id) {
                let reaped = self.stop(id, true);
                self.lifecycle.stop(id, reaped.clone(), Some(id * 10));
                self.stopping.insert(id, reaped);
            }
        }

        fn cancel(&mut self, id: i64) {
            if let Some(start) = self.reads.get_mut(&id) {
                *start = false;
            }
        }

        /// Applies `event`, or returns false if it can't happen now.
        fn apply(&mut self, event: Event) -> bool {
            match event {
                Event::Open(id) => {
                    if self.open.contains(&id)
                        || self.gone.contains(&id)
                        || self.lifecycle.is_deleting(id)
                        || self.lifecycle.reaping(id).is_some()
                    {
                        return false;
                    }
                    if self.lifecycle.open(id) {
                        assert!(!self.reads.contains_key(&id), "{id} read twice");
                        self.reads.insert(id, true);
                        self.stopping.remove(&id);
                    } else {
                        *self.reads.get_mut(&id).expect("a read to revive") = true;
                    }
                }
                Event::ReadDone(id) => {
                    let Some(expected) = self.reads.remove(&id) else {
                        return false;
                    };
                    let start = self.lifecycle.opened(id);
                    assert_eq!(start, expected, "{id} started when it shouldn't, or not");
                    if start && !self.gone.contains(&id) {
                        assert!(!self.open.contains(&id), "{id} has two CLIs");
                        assert!(self.lifecycle.reaping(id).is_none(), "{id} isn't reaped");
                        assert!(!self.lifecycle.is_deleting(id), "{id} is being deleted");
                        self.open.insert(id);
                    }
                }
                Event::ReadWithoutWindow(id) => {
                    if self.reads.remove(&id).is_none() {
                        return false;
                    }
                    self.lifecycle.opened(id);
                }
                Event::Close(id) => {
                    if !self.open.contains(&id) {
                        return false;
                    }
                    self.close(id);
                }
                Event::Archive(id) => {
                    if self.gone.contains(&id) {
                        return false;
                    }
                    self.lifecycle.cancel_opening(id);
                    self.cancel(id);
                    self.close(id);
                }
                Event::CloseAll => {
                    self.lifecycle.close_all();
                    for id in CHATS {
                        self.cancel(id);
                        self.close(id);
                    }
                }
                Event::Reap(id) => {
                    let Some(stop) = self
                        .stops
                        .iter_mut()
                        .find(|stop| stop.id == id && stop.sender.is_some())
                    else {
                        return false;
                    };
                    stop.sender = None;
                }
                Event::ReapedTask(id) => {
                    let Some(index) = self
                        .stops
                        .iter()
                        .position(|stop| stop.id == id && stop.task && stop.reaped.is_closed())
                    else {
                        return false;
                    };
                    let stop = self.stops.remove(index);
                    self.lifecycle.reaped(id, &stop.reaped);
                    if self
                        .stopping
                        .get(&id)
                        .is_some_and(|reaped| reaped.same_channel(&stop.reaped))
                    {
                        self.stopping.remove(&id);
                    }
                }
                Event::Delete(id) => {
                    if self.gone.contains(&id) {
                        return false;
                    }
                    self.lifecycle.cancel_opening(id);
                    self.cancel(id);
                    let reaped = if self.open.remove(&id) {
                        Some(self.stop(id, false))
                    } else {
                        self.lifecycle.reaping(id)
                    };
                    self.lifecycle.mark_deleting(id, None, reaped);
                    *self.deletes.entry(id).or_default() += 1;
                    self.to_remove.insert(id);
                }
                Event::Saved(id, deleted) => {
                    let Some(pending) = self.deletes.get_mut(&id).filter(|pending| **pending > 0)
                    else {
                        return false;
                    };
                    *pending -= 1;
                    let finished = self.lifecycle.finish_deleting(id, deleted);
                    if finished.is_some() {
                        assert!(deleted, "{id} removed though its delete failed");
                        assert!(self.to_remove.remove(&id), "{id} removed twice");
                    }
                    if deleted {
                        self.gone.insert(id);
                    }
                }
            }
            true
        }

        fn check(&self) {
            let opening: HashSet<i64> = self.lifecycle.opening().collect();
            let reading: HashSet<i64> = self.reads.keys().copied().collect();
            assert_eq!(opening, reading, "opening");
            let stopping: HashSet<i64> = self.lifecycle.stopping().map(|(id, ..)| id).collect();
            let expected: HashSet<i64> = self.stopping.keys().copied().collect();
            assert_eq!(stopping, expected, "stopping");
            for id in CHATS {
                if self.open.contains(&id) {
                    assert!(
                        !opening.contains(&id) && !stopping.contains(&id),
                        "{id} open"
                    );
                }
                assert_eq!(
                    self.lifecycle.reaping(id).is_some(),
                    self.stopping
                        .get(&id)
                        .is_some_and(|reaped| !reaped.is_closed()),
                    "{id} reaping"
                );
                let pending = self.deletes.get(&id).copied().unwrap_or_default();
                if self.lifecycle.is_deleting(id) {
                    assert!(pending > 0, "{id} deleting without a delete under way");
                } else if pending > 0 {
                    // Only a delete saved before the others lets it go.
                    assert!(self.gone.contains(&id), "{id} not deleting");
                }
            }
        }
    }

    fn events() -> Vec<Event> {
        let mut events = vec![Event::CloseAll];
        for id in CHATS {
            events.extend([
                Event::Open(id),
                Event::ReadDone(id),
                Event::ReadWithoutWindow(id),
                Event::Close(id),
                Event::Archive(id),
                Event::Reap(id),
                Event::ReapedTask(id),
                Event::Delete(id),
                Event::Saved(id, true),
                Event::Saved(id, false),
            ]);
        }
        events
    }

    /// Plays every sequence of events up to `depth` long that can happen,
    /// from the chats being `open` or not, checking the lifecycle after each
    /// one.
    fn explore(open: bool, sequence: &mut Vec<Event>, events: &[Event], depth: usize) -> usize {
        let mut played = 1;
        if sequence.len() == depth {
            return played;
        }
        for &event in events {
            let mut world = World::default();
            if open {
                world.open.extend(CHATS);
            }
            for &earlier in sequence.iter() {
                assert!(world.apply(earlier));
            }
            if !world.apply(event) {
                continue;
            }
            sequence.push(event);
            world.check();
            played += explore(open, sequence, events, depth);
            sequence.pop();
        }
        played
    }

    #[test]
    fn holds_in_every_order_of_events() {
        for open in [false, true] {
            let played = explore(open, &mut Vec::new(), &events(), 6);
            assert!(played > 100_000, "{played}");
        }
    }
}

#[cfg(test)]
mod adding_tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use super::{Added, Lifecycle, Saved, Saving};
    use crate::worktree::Worktree;

    fn worktree(n: u32) -> (Worktree, PathBuf) {
        let root = PathBuf::from(format!("/worktrees/{n}"));
        (
            Worktree {
                folder: root.clone(),
                root,
            },
            PathBuf::from("/project"),
        )
    }

    fn saved(closed_all: u64) -> Saved {
        Saved {
            id: Ok(Some(1)),
            kept: Ok(true),
            closed_all,
            project_removing: false,
            project_archived: false,
            chat_archived: false,
            chat_open: false,
            window: true,
        }
    }

    #[test]
    fn starts_a_new_chat_only_when_nothing_says_otherwise() {
        let ids = [Ok(Some(1)), Ok(None), Err("no".to_string())];
        let kepts = [Ok(true), Ok(false), Err("no".to_string())];
        for id in &ids {
            for kept in &kepts {
                for flags in 0..1u32 << 9 {
                    let flag = |n: u32| flags & 1 << n != 0;
                    let (closed_all, archived, deleting, opening) =
                        (flag(0), flag(1), flag(2), flag(3));
                    let mut lifecycle = Lifecycle::default();
                    let saving = lifecycle.save(Some(worktree(1)));
                    if closed_all {
                        lifecycle.close_all();
                    }
                    if archived {
                        lifecycle.archive_project();
                    }
                    if deleting {
                        lifecycle.mark_deleting(1, None, None);
                    }
                    if opening {
                        lifecycle.open(1);
                    }
                    let saved = Saved {
                        id: id.clone(),
                        kept: kept.clone(),
                        closed_all: 0,
                        project_removing: flag(4),
                        project_archived: flag(5),
                        chat_archived: flag(6),
                        chat_open: flag(7),
                        window: flag(8),
                    };
                    let expected = match (id, kept) {
                        (Ok(None), _) => Added::Lost,
                        (Err(err), _) => Added::Failed(err.clone()),
                        _ if saved.project_removing => Added::Delete(1),
                        (_, Ok(false)) => Added::Lost,
                        (_, Err(err)) => Added::Unknown(err.clone()),
                        _ if closed_all
                            || saved.chat_archived
                            || deleting
                            || opening
                            || saved.chat_open
                            || archived && saved.project_archived
                            || !saved.window =>
                        {
                            Added::Listed
                        }
                        _ => Added::Start(1),
                    };
                    let (added, given) = lifecycle.added(saving, saved);
                    assert_eq!(added, expected, "{id:?} {kept:?} {flags:#b}");
                    assert_eq!(given, Some(worktree(1)));
                    assert!(lifecycle.take_adding().is_empty());
                }
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Event {
        /// The user asks for a new chat, in a worktree of its own or not.
        Ask {
            worktree: bool,
        },
        /// Git is done with the oldest checkout, which it made or not.
        CheckedOut(bool),
        /// The oldest add is saved, as asked, after its project went, after
        /// it failed, or after the chat was deleted from a listing.
        Saved(u8),
        CloseAll,
        ArchiveProject,
        RestoreProject,
        Quit,
    }

    /// The workspace around the lifecycle, and who has each worktree.
    #[derive(Default)]
    struct World {
        lifecycle: Lifecycle,
        made: u32,
        /// Checkouts git is still on, with `closed_all` when asked for.
        checkouts: Vec<u64>,
        /// Adds being saved, with `closed_all` when asked for, their
        /// worktree, and whether a project was archived since.
        saving: Vec<(Saving, u64, Option<u32>, bool)>,
        /// Worktrees a chat has, and the ones removed.
        chats: HashSet<u32>,
        removed: HashSet<u32>,
        project_archived: bool,
        quit: bool,
    }

    impl World {
        fn save(&mut self, closed_all: u64, worktree: Option<u32>) {
            let saving = self
                .lifecycle
                .save(worktree.map(super::adding_tests::worktree));
            self.saving.push((saving, closed_all, worktree, false));
        }

        fn apply(&mut self, event: Event) -> bool {
            if self.quit {
                return false;
            }
            match event {
                Event::Ask { worktree } => {
                    let closed_all = self.lifecycle.closed_all();
                    if worktree {
                        self.checkouts.push(closed_all);
                    } else {
                        self.save(closed_all, None);
                    }
                }
                Event::CheckedOut(made) => {
                    if self.checkouts.is_empty() {
                        return false;
                    }
                    let closed_all = self.checkouts.remove(0);
                    if made {
                        self.made += 1;
                        if self.lifecycle.closed_all() != closed_all {
                            self.removed.insert(self.made);
                        } else {
                            self.save(closed_all, Some(self.made));
                        }
                    }
                }
                Event::Saved(how) => {
                    if self.saving.is_empty() {
                        return false;
                    }
                    let (saving, closed_all, made, archived_since) = self.saving.remove(0);
                    let mut saved = saved(closed_all);
                    saved.project_archived = self.project_archived;
                    match how {
                        0 => {}
                        1 => saved.id = Ok(None),
                        2 => saved.id = Err("no".to_string()),
                        _ => saved.kept = Ok(false),
                    }
                    let (added, given) = self.lifecycle.added(saving, saved);
                    assert_eq!(
                        given.map(|(worktree, _)| worktree.root),
                        made.map(|n| super::adding_tests::worktree(n).0.root)
                    );
                    match added {
                        Added::Lost | Added::Failed(_) | Added::Delete(_) => {
                            self.removed.extend(made);
                        }
                        Added::Unknown(_) | Added::Listed => self.chats.extend(made),
                        Added::Start(_) => {
                            assert_eq!(
                                closed_all,
                                self.lifecycle.closed_all(),
                                "started after close-all"
                            );
                            assert!(
                                !(archived_since && self.project_archived),
                                "started in an archived project"
                            );
                            self.chats.extend(made);
                        }
                    }
                }
                Event::CloseAll => self.lifecycle.close_all(),
                Event::ArchiveProject => {
                    self.lifecycle.archive_project();
                    self.project_archived = true;
                    for saving in &mut self.saving {
                        saving.3 = true;
                    }
                }
                Event::RestoreProject => self.project_archived = false,
                Event::Quit => {
                    self.quit = true;
                    let adding: HashSet<PathBuf> = self
                        .lifecycle
                        .take_adding()
                        .into_iter()
                        .map(|(worktree, _)| worktree.root)
                        .collect();
                    let made: Vec<u32> = self
                        .saving
                        .drain(..)
                        .filter_map(|(_, _, made, _)| made)
                        .collect();
                    let saving: HashSet<PathBuf> = made
                        .iter()
                        .map(|&n| super::adding_tests::worktree(n).0.root)
                        .collect();
                    // No chat took them, so shutdown removes them.
                    self.removed.extend(made);
                    assert_eq!(adding, saving, "shutdown's worktrees");
                    // Git still checking out is abandoned, which removes what
                    // it made.
                    self.checkouts.clear();
                }
            }
            true
        }

        fn check(&self) {
            let saving: Vec<u32> = self
                .saving
                .iter()
                .filter_map(|(_, _, made, _)| *made)
                .collect();
            for n in 1..=self.made {
                let owners = [
                    saving.contains(&n),
                    self.chats.contains(&n),
                    self.removed.contains(&n),
                ];
                assert_eq!(
                    owners.iter().filter(|owned| **owned).count(),
                    1,
                    "worktree {n}: {owners:?}"
                );
            }
        }
    }

    fn explore(sequence: &mut Vec<Event>, events: &[Event], depth: usize) -> usize {
        let mut played = 1;
        if sequence.len() == depth {
            return played;
        }
        for &event in events {
            let mut world = World::default();
            for &earlier in sequence.iter() {
                assert!(world.apply(earlier));
            }
            if !world.apply(event) {
                continue;
            }
            sequence.push(event);
            world.check();
            played += explore(sequence, events, depth);
            sequence.pop();
        }
        played
    }

    #[test]
    fn new_chats_hold_in_every_order_of_events() {
        let events = [
            Event::Ask { worktree: true },
            Event::Ask { worktree: false },
            Event::CheckedOut(true),
            Event::CheckedOut(false),
            Event::Saved(0),
            Event::Saved(1),
            Event::Saved(2),
            Event::Saved(3),
            Event::CloseAll,
            Event::ArchiveProject,
            Event::RestoreProject,
            Event::Quit,
        ];
        let played = explore(&mut Vec::new(), &events, 6);
        assert!(played > 100_000, "{played}");
    }
}
