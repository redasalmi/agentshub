//! What the runtime does when the app starts and quits.
//!
//! Nothing can be scheduled on a released runtime, so shutdown runs on the
//! app's thread and reads what the CLIs wrote last itself.

use std::collections::HashSet;
use std::path::Path;

use gpui_kit::{App, Context};

use crate::Runtime;
use crate::chats::take_and_remove;
use crate::terminal;

impl Runtime {
    /// Reads what the CLIs wrote since the transcripts were last read, so
    /// that the search finds it without each chat having to be opened first.
    pub(crate) fn catch_up(&self, cx: &mut Context<Self>) {
        let behind = self.store.transcripts_behind();
        cx.spawn(async move |this, cx| {
            match behind.await {
                Ok(sessions) => this
                    .update(cx, |this, cx| {
                        this.read_sessions(sessions, false, cx).detach()
                    })
                    .ok(),
                Err(err) => {
                    eprintln!("can't list the transcripts to read: {err}");
                    None
                }
            };
        })
        .detach();
    }

    /// Stops every CLI, saves what is queued, and reads the sessions the
    /// CLIs left before they go.
    #[expect(
        clippy::too_many_lines,
        reason = "the shutdown steps run in order, each on what the last took from the lifecycle"
    )]
    pub(crate) fn shutdown(&mut self, cx: &mut App) {
        let store = self.store.clone();
        // The changes the user made are saved before anything else, so that a
        // chat whose delete is saved can have its worktree removed.
        store.wait_for_writes();
        // A removal confirmed while its project's chats were being listed
        // isn't saved yet, so it is here, with their worktrees.
        let removing = self.lifecycle.take_removing();
        // Their chats are gone with them, so nothing is read or recorded for
        // them below. Their CLIs are still stopped.
        let mut removed = HashSet::new();
        if !removing.is_empty()
            && let Ok(projects) = smol::block_on(store.projects())
        {
            for (id, path) in removing {
                let Some(project) = projects
                    .iter()
                    .find(|project| project.id == id && project.path == path)
                else {
                    continue;
                };
                match smol::block_on(store.remove_project(id, path.clone())) {
                    Ok(()) => {
                        for chat in &project.chats {
                            removed.insert(chat.id);
                            if let Some(worktree) = &chat.worktree {
                                self.lifecycle.track_removal(worktree, &path, Vec::new());
                            }
                        }
                    }
                    Err(err) => eprintln!("can't remove project {id} on shutdown: {err:#}"),
                }
            }
        }
        let mut deleted_reaped = Vec::new();
        for (id, deleting) in self.lifecycle.take_deleting() {
            match deleting.worktree {
                Some((worktree, project)) if matches!(smol::block_on(store.chat(id)), Ok(None)) => {
                    self.lifecycle.track_removal(
                        &worktree,
                        &project,
                        deleting.reaped.into_iter().collect(),
                    );
                }
                _ => deleted_reaped.extend(deleting.reaped),
            }
        }
        let adding = self.lifecycle.take_adding();
        if !adding.is_empty()
            && let Ok(projects) = smol::block_on(store.projects())
        {
            let taken: HashSet<&Path> = projects
                .iter()
                .flat_map(|project| &project.chats)
                .filter_map(|chat| Some(chat.worktree.as_ref()?.root.as_path()))
                .collect();
            for (worktree, project) in adding {
                if !taken.contains(worktree.root.as_path()) {
                    self.lifecycle
                        .track_removal(&worktree, &project, Vec::new());
                }
            }
        }
        // Nothing can be scheduled on a released runtime, so what the CLIs
        // wrote last is read here: the sessions in their queued reports, the
        // ones they were in, and the reads still under way. The sessions they
        // left before were read when they left them.
        let mut sessions = self.following.reading.clone();
        sessions.extend(
            self.lifecycle
                .stopping()
                .filter(|(id, ..)| !removed.contains(id))
                .filter_map(|(.., session)| session),
        );
        // A session saved by now may not have reached its chat, whose callback
        // can't run anymore, but it is the chat's last.
        let chats: Vec<i64> = self
            .open_chats()
            .map(|chat| chat.id)
            .chain(self.lifecycle.stopping().map(|(id, ..)| id))
            .chain(self.following.reading_last.iter().copied())
            .filter(|id| !removed.contains(id))
            .collect();
        for id in chats {
            sessions.extend(smol::block_on(self.last_session(id)));
        }
        // A chat being opened reads its last session on a thread of its own,
        // which isn't in `reading`, so it is read again here, the way
        // `open_chat` reads it.
        for id in self.lifecycle.opening().filter(|id| !removed.contains(id)) {
            let listed = smol::block_on(store.sessions(id)).unwrap_or_else(|err| {
                eprintln!("can't list the sessions of chat {id}: {err}");
                Vec::new()
            });
            if let Some(last) = listed.last()
                && last.transcript_path.exists()
            {
                sessions.push(last.id);
            }
        }
        // Discover Codex's last session before its process disappears. A
        // queued watcher callback cannot run once we're released.
        sessions.extend(self.record_codex_sessions(&removed, cx));
        // The CLIs are killed on threads of their own, which the app would
        // otherwise exit before they are done with.
        let reaped: Vec<_> = self
            .open_chats()
            .filter_map(|chat| self.terminal(chat.id))
            .map(|terminal| terminal.read(cx).shutdown())
            .chain(
                self.lifecycle
                    .stopping()
                    .map(|(_, reaped, _)| reaped.clone()),
            )
            .chain(deleted_reaped)
            .chain(self.shut_down_shells(cx))
            .chain(self.lifecycle.removal_waits().cloned())
            .collect();
        terminal::wait_reaped(reaped);
        terminal::stop_all_scopes();
        terminal::wait_scopes_stopped();
        self.abandon_checkouts();
        // The worktrees of deleted chats, whose tasks won't run anymore. One
        // being removed already is waited for.
        for job in self.lifecycle.take_removals() {
            if let Some(Err(err)) = take_and_remove(&job) {
                eprintln!("can't remove a worktree on shutdown: {err:#}");
            }
        }
        self.import_chats(&removed);
        sessions.sort_unstable();
        sessions.dedup();
        for id in sessions {
            if let Err(err) = smol::block_on(store.read_transcript(id)) {
                eprintln!("can't read session {id} on shutdown: {err}");
            }
        }
        // The sessions recorded here left others, which the store reads.
        store.wait_for_writes();
        self.following.reports.remove();
    }
}
