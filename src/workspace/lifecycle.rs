//! Where each chat is on its way in or out: being added, opened, closed once
//! a Codex lookup is done, stopped or deleted, and which may take the view
//! once it's in. Kept apart from the window so every order of events can be
//! tested.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

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

/// A worktree and its project's folder, until someone takes them to remove
/// the worktree. The remover holds the lock while git runs, so shutdown
/// waits for a removal under way rather than quit midway.
pub type RemovalJob = Arc<Mutex<Option<(Worktree, PathBuf)>>>;

/// A worktree to remove once what ran in it has been reaped.
struct Removal {
    job: RemovalJob,
    /// The CLI and the shells that ran in it, until they're reaped.
    waits: Vec<Reaped>,
}

impl Removal {
    /// Whether it still has to be removed, or is being removed.
    fn is_pending(&self) -> bool {
        self.job.try_lock().map_or(true, |job| job.is_some())
    }
}

/// Takes the worktree out of `job` and removes it with `remove`, unless it
/// was taken.
pub fn take_and_remove<T>(
    job: &Mutex<Option<(Worktree, PathBuf)>>,
    remove: impl FnOnce(&Worktree, &Path) -> T,
) -> Option<T> {
    let mut job = job.lock().unwrap_or_else(PoisonError::into_inner);
    let (worktree, project) = job.take()?;
    Some(remove(&worktree, &project))
}

/// A chat being added, until that is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Saving(u64);

/// A chat being added, with its project and the worktree made for it.
struct Adding {
    project: i64,
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

/// The user asking to show a chat, which takes the view once it's open
/// unless something else was asked for meanwhile.
#[derive(Clone, Copy)]
pub struct Request {
    /// `navigations` once asked.
    navigations: u64,
    /// The chat being opened that was asked for before, which the view goes
    /// back to if this one turns out to be gone.
    overtaken: Option<i64>,
}

/// A look for the sessions running Codex CLIs are in among their open
/// files, as Codex doesn't report them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lookup(u64);

/// An open Codex chat's part in those lookups. One can take longer than the
/// next, so an older one that finishes later mustn't move the chat back to
/// a session it has left.
#[derive(Default)]
pub struct Codex {
    /// The lookup that last looked at it.
    lookup: u64,
    /// Whether that lookup is still under way.
    pending: bool,
    /// Whether the chat closes once that lookup has recorded what it found.
    closing: bool,
}

impl Codex {
    /// Closes the chat once the lookup under way has recorded the session
    /// its CLI was in, if one is. Returns whether one is.
    pub fn close_after_lookup(&mut self) -> bool {
        if self.pending {
            self.closing = true;
        }
        self.pending
    }

    /// Whether `lookup` is the last that looked at the chat, so what it
    /// found can be recorded.
    pub fn looked_up_by(&self, lookup: Lookup) -> bool {
        self.lookup == lookup.0
    }

    /// Ends `lookup` for the chat, unless a newer one looks at it, and says
    /// whether the chat closes now.
    pub fn looked_up(&mut self, lookup: Lookup) -> bool {
        if !self.pending || !self.looked_up_by(lookup) {
            return false;
        }
        self.pending = false;
        self.closing
    }
}

#[derive(Default)]
pub struct Lifecycle {
    phases: HashMap<i64, Phase>,
    deleting: HashMap<i64, Deleting>,
    /// The chats whose delete was saved. Ids aren't reused.
    gone: HashSet<i64>,
    adding: HashMap<Saving, Adding>,
    next_saving: u64,
    /// How many times all the chats were closed. A new chat asked for before
    /// then doesn't start.
    closed_all: u64,
    /// How many times a project was archived, which a chat added meanwhile
    /// doesn't start in.
    archived_projects: u64,
    /// The last Codex lookup started.
    codex_lookup: u64,
    /// The projects being removed, with their folders, which a chat added
    /// meanwhile goes with.
    removing: HashMap<i64, PathBuf>,
    /// How many deletes, of a chat or of a project, have been saved.
    deletions: u64,
    /// The worktrees of deleted chats waiting for their CLI and shells to be
    /// reaped. Each is removed by its task, or by shutdown if the app quits
    /// first, so none is left once its chat is gone.
    removals: Vec<Removal>,
    /// The chat the user last asked to show. Opening a chat reads its last
    /// session before starting its CLI, so a request can finish after a newer
    /// one; only the latest one may take the view.
    requested: Option<i64>,
    /// How many times the user has asked to show something: a chat, a
    /// project, a new chat, or the settings, or the chat again in their
    /// place. Something that finishes later than it started, like a chat's
    /// worktree being checked out, only takes the view if this hasn't changed
    /// meanwhile.
    navigations: u64,
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

    /// Keeps chat `id` from starting, and from taking the view, once its last
    /// session is read or its stopped CLI is reaped.
    pub fn cancel_opening(&mut self, id: i64) {
        if let Some(Phase::Opening { start }) = self.phases.get_mut(&id) {
            *start = false;
        }
        if self.requested == Some(id) {
            self.requested = None;
        }
    }

    /// Closing all the chats keeps every chat being opened, or asked for
    /// before, from starting.
    pub fn close_all(&mut self) {
        self.closed_all += 1;
        self.requested = None;
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
        // A listing from before its delete was saved can still have it.
        if self.gone.contains(&id) {
            return;
        }
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

    /// Lets `chats` be opened again once deleting them is done, and gives
    /// what to remove of the ones that were deleted.
    pub fn finish_deleting_all(&mut self, chats: &[i64], deleted: bool) -> Vec<(i64, Deleted)> {
        if deleted {
            self.deletions += 1;
        }
        chats
            .iter()
            .filter_map(|&id| Some((id, self.finish_deleting(id, deleted)?)))
            .collect()
    }

    /// Lets chat `id` be opened again once deleting it is done, and gives
    /// what to remove if it was deleted.
    fn finish_deleting(&mut self, id: i64, deleted: bool) -> Option<Deleted> {
        let deleting = self.deleting.get_mut(&id)?;
        deleting.pending -= 1;
        // Another delete of it may still be saved.
        if !deleted && deleting.pending > 0 {
            return None;
        }
        let Deleting {
            worktree, reaped, ..
        } = self.deleting.remove(&id)?;
        if deleted {
            self.gone.insert(id);
        } else if let Some(reaped) = reaped.clone()
            && !self.phases.contains_key(&id)
        {
            // Its CLI may not be reaped yet, and nothing else waits for it.
            self.phases.insert(
                id,
                Phase::Stopping {
                    reaped,
                    session: None,
                },
            );
        }
        deleted.then_some(Deleted { worktree, reaped })
    }

    pub fn is_deleting(&self, id: i64) -> bool {
        self.deleting.contains_key(&id)
    }

    pub fn deletions(&self) -> u64 {
        self.deletions
    }

    /// Starts removing project `id`, in `path`, unless it's being removed
    /// already, as its id may name another project once that is saved.
    pub fn remove_project(&mut self, id: i64, path: PathBuf) -> bool {
        if self.removing.contains_key(&id) {
            return false;
        }
        self.removing.insert(id, path);
        true
    }

    /// Ends removing project `id`, which is saved, failed, or found gone.
    pub fn project_removed(&mut self, id: i64) {
        self.removing.remove(&id);
    }

    pub fn is_removing_project(&self, id: i64) -> bool {
        self.removing.contains_key(&id)
    }

    /// The projects being removed, which shutdown removes.
    pub fn take_removing(&mut self) -> Vec<(i64, PathBuf)> {
        self.removing.drain().collect()
    }

    /// Has shutdown remove `worktree` once `waits` are reaped, unless the
    /// job this returns is taken first.
    pub fn track_removal(
        &mut self,
        worktree: &Worktree,
        project: &Path,
        waits: Vec<Reaped>,
    ) -> RemovalJob {
        let job = Arc::new(Mutex::new(Some((worktree.clone(), project.to_path_buf()))));
        self.removals.retain(Removal::is_pending);
        self.removals.push(Removal {
            job: job.clone(),
            waits,
        });
        job
    }

    /// What the worktrees still to remove wait for.
    pub fn removal_waits(&self) -> impl Iterator<Item = &Reaped> + '_ {
        self.removals.iter().flat_map(|removal| &removal.waits)
    }

    /// The worktrees still to remove, which shutdown removes once everything
    /// is reaped.
    pub fn take_removals(&mut self) -> Vec<RemovalJob> {
        std::mem::take(&mut self.removals)
            .into_iter()
            .map(|removal| removal.job)
            .collect()
    }

    pub fn closed_all(&self) -> u64 {
        self.closed_all
    }

    /// A project was archived, which stops its chats.
    pub fn archive_project(&mut self) {
        self.archived_projects += 1;
    }

    /// Starts adding a chat to `project`, made the `worktree` of the project
    /// if it has one, until `added` says what then becomes of it.
    pub fn save(&mut self, project: i64, worktree: Option<(Worktree, PathBuf)>) -> Saving {
        let saving = Saving(self.next_saving);
        self.next_saving += 1;
        self.adding.insert(
            saving,
            Adding {
                project,
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
            project,
            worktree,
            archived_projects,
        } = self.adding.remove(&saving).expect("a chat is added once");
        let added = match saved.id {
            Ok(Some(id)) => self.decide(id, project, archived_projects, saved),
            Ok(None) => Added::Lost,
            Err(err) => Added::Failed(err),
        };
        (added, worktree)
    }

    fn decide(&self, id: i64, project: i64, archived_projects: u64, saved: Saved) -> Added {
        // Its project may have been removed while it was added, with the
        // chats it had before this one.
        if self.is_removing_project(project) {
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

    pub fn navigations(&self) -> u64 {
        self.navigations
    }

    /// The user asked to show a project.
    pub fn navigate(&mut self) {
        self.navigations += 1;
    }

    /// Asking for a new chat supersedes the chats still opening, and the new
    /// chats asked for before, which start in the background.
    pub fn ask(&mut self) {
        self.navigations += 1;
        self.requested = None;
    }

    /// The user showed the settings, or the chat again in their place. A
    /// chat still opening mustn't take the view from the settings.
    pub fn show_settings(&mut self, shown: bool) {
        self.navigations += 1;
        if shown {
            self.requested = None;
        }
    }

    /// The user asked to show chat `id`.
    pub fn request(&mut self, id: i64) -> Request {
        self.navigations += 1;
        let overtaken = self.requested.filter(|each| self.is_opening(*each));
        self.requested = Some(id);
        Request {
            navigations: self.navigations,
            overtaken,
        }
    }

    /// Whether chat `id` is the last the user asked to show, so it takes the
    /// view once it's open.
    pub fn is_requested(&self, id: i64) -> bool {
        self.requested == Some(id)
    }

    /// Chat `id`, asked for by `request`, turned out to be gone. Gives the
    /// chat it took the request over from, which is asked for again, unless
    /// something else was asked for since.
    pub fn give_back(&mut self, id: i64, request: Request) -> Option<i64> {
        if !self.is_requested(id) || self.navigations != request.navigations {
            return None;
        }
        let overtaken = request.overtaken?;
        self.requested = Some(overtaken);
        Some(overtaken)
    }

    /// Chat `id` was shown.
    pub fn shown(&mut self, id: i64) {
        self.requested = Some(id);
        self.navigations += 1;
    }

    /// Starts a Codex lookup of `chats`, unless there are none.
    pub fn look_up_codex<'a>(
        &mut self,
        chats: impl IntoIterator<Item = &'a mut Codex>,
    ) -> Option<Lookup> {
        let lookup = self.codex_lookup.wrapping_add(1);
        let mut any = false;
        for codex in chats {
            codex.lookup = lookup;
            codex.pending = true;
            any = true;
        }
        any.then(|| {
            self.codex_lookup = lookup;
            Lookup(lookup)
        })
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
        /// What the CLI stopped by a delete under way says once it's reaped.
        deleting_reaped: HashMap<i64, Reaped>,
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
                    if !self.lifecycle.is_deleting(id) {
                        self.deleting_reaped.remove(&id);
                    }
                    if let Some(reaped) = &reaped {
                        self.deleting_reaped.insert(id, reaped.clone());
                    }
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
                    let was_deleting = self.lifecycle.is_deleting(id);
                    let finished = self.lifecycle.finish_deleting(id, deleted);
                    if finished.is_some() {
                        assert!(deleted, "{id} removed though its delete failed");
                        assert!(self.to_remove.remove(&id), "{id} removed twice");
                    }
                    if deleted {
                        self.gone.insert(id);
                    } else if was_deleting
                        && !self.lifecycle.is_deleting(id)
                        && let Some(reaped) = self.deleting_reaped.remove(&id)
                        && !self.reads.contains_key(&id)
                        && !self.stopping.contains_key(&id)
                    {
                        // The CLI a failed delete stopped is still waited for.
                        self.stopping.insert(id, reaped);
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
                    let saving = lifecycle.save(7, Some(worktree(1)));
                    let project_removing = flag(4);
                    if project_removing {
                        lifecycle.remove_project(7, PathBuf::from("/project"));
                    }
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
                        project_archived: flag(5),
                        chat_archived: flag(6),
                        chat_open: flag(7),
                        window: flag(8),
                    };
                    let expected = match (id, kept) {
                        (Ok(None), _) => Added::Lost,
                        (Err(err), _) => Added::Failed(err.clone()),
                        _ if project_removing => Added::Delete(1),
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
                .save(1, worktree.map(super::adding_tests::worktree));
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

#[cfg(test)]
mod codex_tests {
    use super::{Codex, Lifecycle, Lookup};

    #[derive(Clone, Copy, Debug)]
    enum Event {
        /// A lookup starts, of the running chats in the mask.
        LookUp(u8),
        /// The lookup started that many before the newest still under way
        /// finishes.
        Finish(usize),
        /// The chat's CLI is started, as on opening or restarting it.
        Open(i64),
        /// The user quits the chat's CLI.
        Quit(i64),
        /// The chat is closed, and its CLI's open files show no session.
        Suspend(i64),
        /// The chat's CLI is stopped without waiting, as on deleting it.
        Stop(i64),
    }

    struct Chat {
        id: i64,
        /// Which of the chat's CLIs this is.
        terminal: u32,
        codex: Codex,
        /// Whether its CLI has exited, so no lookup sees it.
        quit: bool,
        /// Whether it should close once nothing more can be found.
        wants_close: bool,
    }

    #[derive(Default)]
    struct World {
        lifecycle: Lifecycle,
        chats: Vec<Chat>,
        terminals: u32,
        /// Lookups under way, oldest first, with the CLIs they look at.
        lookups: Vec<(Lookup, Vec<(i64, u32)>)>,
        /// The newest lookup that looked at each CLI.
        newest: Vec<((i64, u32), Lookup)>,
    }

    impl World {
        fn chat(&mut self, id: i64) -> Option<&mut Chat> {
            self.chats.iter_mut().find(|chat| chat.id == id)
        }

        fn newest(&self, cli: (i64, u32)) -> Option<Lookup> {
            self.newest
                .iter()
                .rev()
                .find(|(each, _)| *each == cli)
                .map(|(_, lookup)| *lookup)
        }

        fn under_way(&self, cli: (i64, u32)) -> bool {
            self.newest(cli).is_some_and(|newest| {
                self.lookups
                    .iter()
                    .any(|(lookup, clis)| *lookup == newest && clis.contains(&cli))
            })
        }

        fn close(&mut self, id: i64) {
            self.chats.retain(|chat| chat.id != id);
        }

        fn apply(&mut self, event: Event) -> bool {
            match event {
                Event::LookUp(mask) => {
                    let mut clis = Vec::new();
                    let looked_at = self.chats.iter_mut().filter_map(|chat| {
                        if chat.quit || mask & 1 << chat.id == 0 {
                            return None;
                        }
                        clis.push((chat.id, chat.terminal));
                        Some(&mut chat.codex)
                    });
                    let lookup = self.lifecycle.look_up_codex(looked_at);
                    assert_eq!(lookup.is_some(), !clis.is_empty());
                    let Some(lookup) = lookup else {
                        return false;
                    };
                    assert!(
                        self.lookups.iter().all(|(each, _)| *each != lookup),
                        "two lookups under way are the same"
                    );
                    self.newest.extend(clis.iter().map(|&cli| (cli, lookup)));
                    self.lookups.push((lookup, clis));
                }
                Event::Finish(back) => {
                    let Some(index) = self.lookups.len().checked_sub(back + 1) else {
                        return false;
                    };
                    let (lookup, clis) = self.lookups.remove(index);
                    for cli in clis {
                        let recorded = self
                            .chats
                            .iter()
                            .find(|chat| (chat.id, chat.terminal) == cli)
                            .is_some_and(|chat| chat.codex.looked_up_by(lookup));
                        assert_eq!(
                            recorded,
                            self.chats
                                .iter()
                                .any(|chat| (chat.id, chat.terminal) == cli)
                                && self.newest(cli) == Some(lookup),
                            "recorded what a lookup that isn't the newest found"
                        );
                    }
                    let mut closing = Vec::new();
                    for chat in &mut self.chats {
                        if chat.codex.looked_up(lookup) {
                            closing.push(chat.id);
                        }
                    }
                    for id in closing {
                        let chat = self.chat(id).expect("a chat closing is open");
                        assert!(chat.wants_close, "closed a chat nobody closed");
                        self.close(id);
                    }
                }
                Event::Open(id) => {
                    if self.chat(id).is_some() {
                        return false;
                    }
                    self.terminals += 1;
                    self.chats.push(Chat {
                        id,
                        terminal: self.terminals,
                        codex: Codex::default(),
                        quit: false,
                        wants_close: false,
                    });
                }
                Event::Quit(id) => {
                    let Some(chat) = self.chat(id).filter(|chat| !chat.quit) else {
                        return false;
                    };
                    chat.quit = true;
                    chat.wants_close = true;
                    if !chat.codex.close_after_lookup() {
                        self.close(id);
                    }
                }
                Event::Suspend(id) => {
                    let Some(chat) = self.chat(id) else {
                        return false;
                    };
                    chat.wants_close = true;
                    if !chat.codex.close_after_lookup() {
                        self.close(id);
                    }
                }
                Event::Stop(id) => {
                    if self.chat(id).is_none() {
                        return false;
                    }
                    self.close(id);
                }
            }
            true
        }

        fn check(&self) {
            for chat in &self.chats {
                let cli = (chat.id, chat.terminal);
                assert_eq!(
                    chat.codex.pending,
                    self.under_way(cli),
                    "pending doesn't match the newest lookup of {cli:?}"
                );
                assert_eq!(
                    chat.wants_close, chat.codex.closing,
                    "closing doesn't match what was asked of {cli:?}"
                );
                // Otherwise nothing would ever close it.
                assert!(
                    !chat.wants_close || chat.codex.pending,
                    "{cli:?} stays open"
                );
            }
        }
    }

    fn events() -> Vec<Event> {
        let mut events = vec![
            Event::LookUp(0b010),
            Event::LookUp(0b100),
            Event::LookUp(0b110),
        ];
        events.extend([0, 1, 2].map(Event::Finish));
        for id in [1, 2] {
            events.extend([
                Event::Open(id),
                Event::Quit(id),
                Event::Suspend(id),
                Event::Stop(id),
            ]);
        }
        events
    }

    fn explore(played: &mut Vec<Event>, events: &[Event], depth: usize) -> usize {
        let mut count = 1;
        if depth == 0 {
            return count;
        }
        for &event in events {
            let mut next = World::default();
            next.apply(Event::Open(1));
            next.apply(Event::Open(2));
            for &event in played.iter() {
                next.apply(event);
            }
            if !next.apply(event) {
                continue;
            }
            next.check();
            played.push(event);
            count += explore(played, events, depth - 1);
            played.pop();
        }
        count
    }

    #[test]
    fn codex_lookups_hold_in_every_order_of_events() {
        let played = explore(&mut Vec::new(), &events(), 6);
        assert!(played > 100_000, "{played}");
    }
}

#[cfg(test)]
mod view_tests {
    use std::collections::HashSet;

    use super::{Lifecycle, Request};

    /// What the user, or a chat taking the view, did.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Act {
        Open(i64),
        Shown(i64),
        GaveBack(i64),
        Ask,
        Settings(bool),
        Select,
        Cancel(i64),
        CloseAll,
    }

    impl Act {
        /// Whether it is the user asking to show something, which a new chat
        /// asked for before mustn't take the view from.
        fn navigates(self) -> bool {
            matches!(
                self,
                Act::Open(_) | Act::Shown(_) | Act::Ask | Act::Settings(_) | Act::Select
            )
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Event {
        Open(i64),
        /// The read of that many before the newest finishes, with the chat
        /// found or gone.
        Read(usize, bool),
        Ask,
        /// The oldest new chat is added.
        Added,
        Settings(bool),
        Select,
        /// The chat is archived or deleted.
        Cancel(i64),
        CloseAll,
        Close(i64),
        /// The chat, open without a CLI, starts a new session and shows.
        Restart(i64),
    }

    #[derive(Default)]
    struct World {
        lifecycle: Lifecycle,
        acts: Vec<Act>,
        /// Last sessions being read, with the request and the act that
        /// started each.
        reads: Vec<(i64, Request, usize)>,
        /// New chats being added, with the act that asked for each, and
        /// `closed_all` and `navigations` then.
        asks: Vec<(usize, u64, u64)>,
        added: i64,
        open: HashSet<i64>,
        shown: Option<i64>,
    }

    impl World {
        /// The chat the user last asked to show, from what happened.
        fn target(&self, acts: &[Act]) -> Option<i64> {
            acts.iter().fold(None, |target, act| match *act {
                Act::Open(id) | Act::Shown(id) | Act::GaveBack(id) => Some(id),
                Act::Ask | Act::Settings(true) | Act::CloseAll => None,
                Act::Cancel(id) if target == Some(id) => None,
                _ => target,
            })
        }

        fn navigated_since(&self, act: usize) -> bool {
            self.acts[act + 1..].iter().any(|act| act.navigates())
        }

        fn show(&mut self, id: i64) {
            assert!(
                self.open.contains(&id),
                "showed chat {id}, which isn't open"
            );
            self.lifecycle.shown(id);
            self.acts.push(Act::Shown(id));
            self.shown = Some(id);
        }

        fn apply(&mut self, event: Event) -> bool {
            match event {
                Event::Open(id) => {
                    let opening: Vec<i64> = self.reads.iter().map(|read| read.0).collect();
                    let before = self.target(&self.acts);
                    self.acts.push(Act::Open(id));
                    let request = self.lifecycle.request(id);
                    assert_eq!(
                        request.overtaken,
                        before.filter(|each| opening.contains(each)),
                        "took over from the wrong chat"
                    );
                    if self.open.contains(&id) {
                        self.show(id);
                    } else if self.lifecycle.open(id) {
                        self.reads.push((id, request, self.acts.len() - 1));
                    }
                }
                Event::Read(back, gone) => {
                    let Some(index) = self.reads.len().checked_sub(back + 1) else {
                        return false;
                    };
                    let (id, request, act) = self.reads.remove(index);
                    if !self.lifecycle.opened(id) {
                        return true;
                    }
                    if gone {
                        let expected =
                            if self.target(&self.acts) == Some(id) && !self.navigated_since(act) {
                                request.overtaken
                            } else {
                                None
                            };
                        let given = self.lifecycle.give_back(id, request);
                        assert_eq!(given, expected, "gave the view back wrongly");
                        if let Some(overtaken) = given {
                            self.acts.push(Act::GaveBack(overtaken));
                            if self.open.contains(&overtaken) {
                                self.show(overtaken);
                            }
                        }
                    } else {
                        self.open.insert(id);
                        let show = self.lifecycle.is_requested(id);
                        assert_eq!(
                            show,
                            self.target(&self.acts) == Some(id),
                            "chat {id} took the view wrongly"
                        );
                        if show {
                            self.show(id);
                        }
                    }
                }
                Event::Ask => {
                    self.acts.push(Act::Ask);
                    self.lifecycle.ask();
                    self.asks.push((
                        self.acts.len() - 1,
                        self.lifecycle.closed_all(),
                        self.lifecycle.navigations(),
                    ));
                }
                Event::Added => {
                    if self.asks.is_empty() {
                        return false;
                    }
                    let (act, closed_all, navigations) = self.asks.remove(0);
                    if self.lifecycle.closed_all() != closed_all {
                        return true;
                    }
                    self.added += 1;
                    let id = 10 + self.added;
                    self.open.insert(id);
                    let show = self.lifecycle.navigations() == navigations;
                    assert_eq!(
                        show,
                        !self.navigated_since(act),
                        "new chat {id} took the view wrongly"
                    );
                    if show {
                        self.show(id);
                    }
                }
                Event::Settings(shown) => {
                    self.acts.push(Act::Settings(shown));
                    self.lifecycle.show_settings(shown);
                }
                Event::Select => {
                    self.acts.push(Act::Select);
                    self.lifecycle.navigate();
                }
                Event::Cancel(id) => {
                    self.acts.push(Act::Cancel(id));
                    self.lifecycle.cancel_opening(id);
                    self.open.remove(&id);
                }
                Event::CloseAll => {
                    self.acts.push(Act::CloseAll);
                    self.lifecycle.close_all();
                    self.open.clear();
                    self.shown = None;
                }
                Event::Restart(id) => {
                    if !self.open.contains(&id) {
                        return false;
                    }
                    self.show(id);
                }
                Event::Close(id) => {
                    if !self.open.remove(&id) {
                        return false;
                    }
                    if self.shown == Some(id) {
                        self.shown = None;
                    }
                }
            }
            true
        }

        fn check(&self) {
            let target = self.target(&self.acts);
            for id in [1, 2] {
                assert_eq!(
                    self.lifecycle.is_requested(id),
                    target == Some(id),
                    "{:?}",
                    self.acts
                );
            }
        }
    }

    fn events() -> Vec<Event> {
        let mut events = vec![
            Event::Read(0, false),
            Event::Read(1, false),
            Event::Read(0, true),
            Event::Read(1, true),
            Event::Ask,
            Event::Added,
            Event::Settings(true),
            Event::Settings(false),
            Event::Select,
            Event::CloseAll,
            Event::Restart(1),
        ];
        for id in [1, 2] {
            events.extend([Event::Open(id), Event::Cancel(id), Event::Close(id)]);
        }
        events
    }

    fn explore(played: &mut Vec<Event>, events: &[Event], depth: usize) -> usize {
        let mut count = 1;
        if depth == 0 {
            return count;
        }
        for &event in events {
            let mut world = World::default();
            for &event in played.iter() {
                world.apply(event);
            }
            if !world.apply(event) {
                continue;
            }
            world.check();
            played.push(event);
            count += explore(played, events, depth - 1);
            played.pop();
        }
        count
    }

    #[test]
    fn only_the_latest_request_takes_the_view_in_every_order_of_events() {
        let played = explore(&mut Vec::new(), &events(), 6);
        assert!(played > 100_000, "{played}");
    }
}

#[cfg(test)]
mod deleting_tests {
    use std::path::PathBuf;

    use super::Lifecycle;
    use crate::worktree::Worktree;

    fn worktree() -> (Worktree, PathBuf) {
        let root = PathBuf::from("/worktrees/1");
        (
            Worktree {
                folder: root.clone(),
                root,
            },
            PathBuf::from("/project"),
        )
    }

    #[test]
    fn a_failed_delete_still_waits_for_the_cli_it_stopped() {
        let mut lifecycle = Lifecycle::default();
        let (_cli, reaped) = smol::channel::bounded::<()>(1);
        lifecycle.mark_deleting(1, Some(worktree()), Some(reaped.clone()));
        assert!(lifecycle.finish_deleting_all(&[1], false).is_empty());
        // The chat doesn't open again, nor does a delete forget it, until
        // the CLI is reaped.
        let waits = lifecycle.reaping(1).expect("the CLI is still waited for");
        assert!(waits.same_channel(&reaped));
        lifecycle.mark_deleting(1, Some(worktree()), lifecycle.reaping(1));
        let deleted = lifecycle.finish_deleting_all(&[1], true);
        let reaped_after = deleted[0].1.reaped.as_ref().expect("waits for the CLI");
        assert!(reaped_after.same_channel(&reaped));
    }

    #[test]
    fn a_chat_whose_delete_is_saved_isnt_deleted_again() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.mark_deleting(1, Some(worktree()), None);
        assert_eq!(lifecycle.finish_deleting_all(&[1], true).len(), 1);
        // As from a project's listing made before that delete was saved.
        lifecycle.mark_deleting(1, Some(worktree()), None);
        assert!(!lifecycle.is_deleting(1));
        assert!(lifecycle.finish_deleting_all(&[1], true).is_empty());
    }
}

#[cfg(test)]
mod removal_tests {
    use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
    use std::path::PathBuf;

    use smol::channel::Sender;

    use super::{Added, Lifecycle, Reaped, RemovalJob, Saved, Saving, take_and_remove};
    use crate::worktree::Worktree;

    const PROJECT: i64 = 1;
    /// The chat that is added, if one is asked for.
    const NEW: i64 = 3;

    fn worktree(chat: i64) -> (Worktree, PathBuf) {
        let root = PathBuf::from(format!("/worktrees/{chat}"));
        (
            Worktree {
                folder: root.clone(),
                root,
            },
            PathBuf::from("/project"),
        )
    }

    fn chat_of(worktree: &Worktree) -> i64 {
        worktree
            .root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[derive(Clone, Copy, Debug)]
    enum Event {
        Delete(i64),
        RemoveProject,
        /// A new chat is asked for, in a worktree of its own.
        Ask,
        /// The store gets to the next thing asked of it: a write, which
        /// works or fails, or a read.
        Store(bool),
        /// The oldest listing for a project removal reaches the workspace.
        Listed,
        /// The oldest check that a new chat is still there reaches it.
        Kept,
        Reap(i64),
        /// The oldest removal whose CLI and shells are reaped runs.
        Removal,
        Quit,
    }

    enum Op {
        Delete(i64),
        RemoveProject(Vec<i64>),
        Add(Saving),
        /// Lists the projects for a removal.
        List,
        /// Checks that the new chat is there, with `deletions` when asked.
        Check(Saving, u64),
    }

    /// The database: whether the project is there, and its chats, with
    /// whether each has a worktree.
    #[derive(Clone)]
    struct Store {
        project: bool,
        rows: BTreeMap<i64, bool>,
    }

    struct World {
        lifecycle: Lifecycle,
        store: Store,
        ops: VecDeque<Op>,
        listings: VecDeque<Store>,
        checks: VecDeque<(Saving, u64, bool)>,
        asked: bool,
        /// The CLIs running, and the ones stopped but not reaped.
        running: HashMap<i64, (Sender<()>, Reaped)>,
        stopped: HashMap<i64, Sender<()>>,
        /// Removals waiting for their CLI and shells to be reaped.
        removals: Vec<(RemovalJob, Vec<Reaped>)>,
        /// The worktrees removed, by chat.
        removed: HashSet<i64>,
        quit: bool,
        played: Vec<Event>,
    }

    impl World {
        fn new() -> Self {
            let (tx, rx) = smol::channel::bounded(1);
            World {
                lifecycle: Lifecycle::default(),
                store: Store {
                    project: true,
                    rows: BTreeMap::from([(1, true), (2, true)]),
                },
                ops: VecDeque::new(),
                listings: VecDeque::new(),
                checks: VecDeque::new(),
                asked: false,
                running: HashMap::from([(1, (tx, rx))]),
                stopped: HashMap::new(),
                removals: Vec::new(),
                removed: HashSet::new(),
                quit: false,
                played: Vec::new(),
            }
        }

        fn worktree_of(&self, chat: i64) -> Option<(Worktree, PathBuf)> {
            self.store
                .rows
                .get(&chat)
                .is_some_and(|has| *has)
                .then(|| worktree(chat))
        }

        fn stop(&mut self, chat: i64) -> Option<Reaped> {
            let (tx, rx) = self.running.remove(&chat)?;
            self.stopped.insert(chat, tx);
            Some(rx)
        }

        fn remove(&mut self, job: &RemovalJob) {
            let Some(chat) = take_and_remove(job, |worktree, _| chat_of(worktree)) else {
                return;
            };
            // Shutdown can come back to a worktree it removed, which only
            // prunes git's record of it.
            assert!(
                self.removed.insert(chat) || self.quit,
                "worktree {chat} removed twice: {:?}",
                self.played
            );
            assert!(
                !self.store.rows.contains_key(&chat),
                "worktree {chat} removed though its chat is there: {:?}",
                self.played
            );
            assert!(
                !self.running.contains_key(&chat) && !self.stopped.contains_key(&chat),
                "worktree {chat} removed before its CLI was reaped: {:?}",
                self.played
            );
        }

        fn remove_worktree(&mut self, worktree: &(Worktree, PathBuf), reaped: Option<Reaped>) {
            let waits: Vec<Reaped> = reaped.into_iter().collect();
            let job = self
                .lifecycle
                .track_removal(&worktree.0, &worktree.1, waits.clone());
            self.removals.push((job, waits));
        }

        fn delete_chat(&mut self, chat: i64, worktree: Option<(Worktree, PathBuf)>) {
            self.lifecycle.cancel_opening(chat);
            let reaped = self.stop(chat).or_else(|| self.lifecycle.reaping(chat));
            self.lifecycle.mark_deleting(chat, worktree, reaped);
            self.ops.push_back(Op::Delete(chat));
        }

        fn finish_deleting(&mut self, chats: &[i64], deleted: bool) {
            for (_, deleted) in self.lifecycle.finish_deleting_all(chats, deleted) {
                if let Some(worktree) = &deleted.worktree {
                    self.remove_worktree(worktree, deleted.reaped);
                }
            }
        }

        fn finish_adding(&mut self, saving: Saving, id: Result<Option<i64>, String>, kept: bool) {
            let saved = Saved {
                id,
                kept: Ok(kept),
                closed_all: self.lifecycle.closed_all(),
                project_archived: false,
                chat_archived: false,
                chat_open: false,
                window: true,
            };
            let (added, worktree) = self.lifecycle.added(saving, saved);
            match added {
                Added::Lost | Added::Failed(_) => {
                    if let Some(worktree) = &worktree {
                        self.remove_worktree(worktree, None);
                    }
                }
                Added::Delete(id) => self.delete_chat(id, worktree),
                Added::Unknown(_) | Added::Listed => {}
                Added::Start(id) => {
                    assert!(
                        !self.lifecycle.is_removing_project(PROJECT),
                        "started a chat in a project being removed"
                    );
                    assert!(
                        self.store.rows.contains_key(&id),
                        "started a chat that's gone"
                    );
                    self.running.insert(id, smol::channel::bounded(1));
                }
            }
        }

        fn apply(&mut self, event: Event) -> bool {
            if self.quit {
                return false;
            }
            self.played.push(event);
            match event {
                Event::Delete(chat) => {
                    if !self.store.rows.contains_key(&chat) {
                        return false;
                    }
                    let worktree = self.worktree_of(chat);
                    self.delete_chat(chat, worktree);
                }
                Event::RemoveProject => {
                    if !self.store.project
                        || !self
                            .lifecycle
                            .remove_project(PROJECT, PathBuf::from("/project"))
                    {
                        return false;
                    }
                    self.ops.push_back(Op::List);
                }
                Event::Ask => {
                    if self.asked || !self.store.project {
                        return false;
                    }
                    self.asked = true;
                    let saving = self.lifecycle.save(PROJECT, Some(worktree(NEW)));
                    self.ops.push_back(Op::Add(saving));
                }
                Event::Store(works) => {
                    let Some(op) = self.ops.pop_front() else {
                        return false;
                    };
                    match op {
                        Op::List | Op::Check(..) if !works => {
                            self.ops.push_front(op);
                            return false;
                        }
                        Op::List => self.listings.push_back(self.store.clone()),
                        Op::Check(saving, deletions) => {
                            let kept = self.store.rows.contains_key(&NEW);
                            self.checks.push_back((saving, deletions, kept));
                        }
                        Op::Delete(chat) => {
                            if works {
                                self.store.rows.remove(&chat);
                            }
                            self.finish_deleting(&[chat], works);
                        }
                        Op::RemoveProject(chats) => {
                            if works {
                                self.store.project = false;
                                self.store.rows.clear();
                            }
                            self.lifecycle.project_removed(PROJECT);
                            self.finish_deleting(&chats, works);
                        }
                        Op::Add(saving) => {
                            if !works {
                                self.finish_adding(saving, Err("no".to_string()), true);
                            } else if self.store.project {
                                self.store.rows.insert(NEW, true);
                                self.ops
                                    .push_back(Op::Check(saving, self.lifecycle.deletions()));
                            } else {
                                self.finish_adding(saving, Ok(None), true);
                            }
                        }
                    }
                }
                Event::Listed => {
                    let Some(listing) = self.listings.pop_front() else {
                        return false;
                    };
                    if !listing.project {
                        self.lifecycle.project_removed(PROJECT);
                        return true;
                    }
                    let chats: Vec<i64> = listing.rows.keys().copied().collect();
                    for (&chat, &has) in &listing.rows {
                        self.lifecycle.cancel_opening(chat);
                        let reaped = self.stop(chat).or_else(|| self.lifecycle.reaping(chat));
                        let worktree = has.then(|| worktree(chat));
                        self.lifecycle.mark_deleting(chat, worktree, reaped);
                    }
                    self.ops.push_back(Op::RemoveProject(chats));
                }
                Event::Kept => {
                    let Some((saving, deletions, kept)) = self.checks.pop_front() else {
                        return false;
                    };
                    if self.lifecycle.deletions() != deletions {
                        self.ops
                            .push_back(Op::Check(saving, self.lifecycle.deletions()));
                    } else {
                        self.finish_adding(saving, Ok(Some(NEW)), kept);
                    }
                }
                Event::Reap(chat) => {
                    if self.stopped.remove(&chat).is_none() {
                        return false;
                    }
                }
                Event::Removal => {
                    let Some(index) = self
                        .removals
                        .iter()
                        .position(|(_, waits)| waits.iter().all(Reaped::is_closed))
                    else {
                        return false;
                    };
                    let (job, _) = self.removals.remove(index);
                    self.remove(&job);
                }
                Event::Quit => self.quit(),
            }
            true
        }

        /// Shutdown, as `Workspace`'s release does it.
        fn quit(&mut self) {
            self.quit = true;
            // The writes asked for are done, but nothing hears of them.
            for op in std::mem::take(&mut self.ops) {
                match op {
                    Op::Delete(chat) => {
                        self.store.rows.remove(&chat);
                    }
                    Op::RemoveProject(_) => {
                        self.store.project = false;
                        self.store.rows.clear();
                    }
                    Op::Add(_) if self.store.project => {
                        self.store.rows.insert(NEW, true);
                    }
                    Op::Add(_) | Op::List | Op::Check(..) => {}
                }
            }
            for _ in self.lifecycle.take_removing() {
                if self.store.project {
                    self.store.project = false;
                    for (chat, has) in std::mem::take(&mut self.store.rows) {
                        if has {
                            let (worktree, project) = worktree(chat);
                            self.lifecycle
                                .track_removal(&worktree, &project, Vec::new());
                        }
                    }
                }
            }
            for (chat, deleted) in self.lifecycle.take_deleting() {
                if let Some((worktree, project)) = deleted.worktree
                    && !self.store.rows.contains_key(&chat)
                {
                    self.lifecycle.track_removal(
                        &worktree,
                        &project,
                        deleted.reaped.into_iter().collect(),
                    );
                }
            }
            for (worktree, project) in self.lifecycle.take_adding() {
                if !self.store.rows.contains_key(&chat_of(&worktree)) {
                    self.lifecycle
                        .track_removal(&worktree, &project, Vec::new());
                }
            }
            // Every CLI is killed, and shutdown waits for them to be reaped.
            self.running.clear();
            self.stopped.clear();
            assert!(self.lifecycle.removal_waits().all(Reaped::is_closed));
            for job in self.lifecycle.take_removals() {
                self.remove(&job);
            }
            for chat in [1, 2, NEW] {
                let made = chat != NEW || self.asked;
                assert_eq!(
                    self.removed.contains(&chat),
                    made && !self.store.rows.contains_key(&chat),
                    "worktree {chat} left behind or removed wrongly"
                );
            }
        }
    }

    fn events() -> Vec<Event> {
        vec![
            Event::Delete(1),
            Event::Delete(2),
            Event::Delete(NEW),
            Event::RemoveProject,
            Event::Ask,
            Event::Store(true),
            Event::Store(false),
            Event::Listed,
            Event::Kept,
            Event::Reap(1),
            Event::Reap(NEW),
            Event::Removal,
            Event::Quit,
        ]
    }

    fn explore(played: &mut Vec<Event>, events: &[Event], depth: usize) -> usize {
        let mut count = 1;
        if depth == 0 {
            return count;
        }
        for &event in events {
            let mut world = World::new();
            for &event in played.iter() {
                world.apply(event);
            }
            if !world.apply(event) {
                continue;
            }
            played.push(event);
            count += explore(played, events, depth - 1);
            played.pop();
        }
        count
    }

    /// Quitting after every sequence, so what's left is checked too.
    fn explore_then_quit(played: &mut Vec<Event>, events: &[Event], depth: usize) {
        let mut world = World::new();
        for &event in played.iter() {
            world.apply(event);
        }
        world.apply(Event::Quit);
        if depth == 0 {
            return;
        }
        for &event in events {
            let mut world = World::new();
            for &event in played.iter() {
                world.apply(event);
            }
            if !world.apply(event) {
                continue;
            }
            played.push(event);
            explore_then_quit(played, events, depth - 1);
            played.pop();
        }
    }

    #[test]
    fn worktrees_go_once_and_only_with_their_chat_in_every_order_of_events() {
        let played = explore(&mut Vec::new(), &events(), 7);
        assert!(played > 100_000, "{played}");
        explore_then_quit(&mut Vec::new(), &events(), 7);
    }
}
