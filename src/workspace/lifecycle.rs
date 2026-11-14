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

#[derive(Default)]
pub struct Lifecycle {
    phases: HashMap<i64, Phase>,
    deleting: HashMap<i64, Deleting>,
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

    /// Keeps every chat being opened from starting.
    pub fn cancel_all_opening(&mut self) {
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
                    self.lifecycle.cancel_all_opening();
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
