//! What the workspace does when the app quits.
//!
//! Nothing can be scheduled on a released workspace, so this runs on the app's
//! thread and reads what the CLIs wrote last itself.

use super::*;

impl Workspace {
    /// Stops every CLI, saves what is queued, and reads the sessions the
    /// CLIs left before they go.
    #[expect(
        clippy::too_many_lines,
        reason = "the shutdown steps run in order, each on what the last took from the lifecycle"
    )]
    pub(super) fn shutdown(&mut self, cx: &mut App) {
        // The changes the user made are saved before anything else, so that a
        // chat whose delete is saved can have its worktree removed.
        self.store.wait_for_writes();
        // A removal confirmed while its project's chats were being listed
        // isn't saved yet, so it is here, with their worktrees.
        let removing = self.lifecycle.take_removing();
        // Their chats are gone with them, so nothing is read or recorded for
        // them below. Their CLIs are still stopped.
        let mut removed = HashSet::new();
        if !removing.is_empty()
            && let Ok(projects) = smol::block_on(self.store.projects())
        {
            for (id, path) in removing {
                let Some(project) = projects
                    .iter()
                    .find(|project| project.id == id && project.path == path)
                else {
                    continue;
                };
                match smol::block_on(self.store.remove_project(id, path.clone())) {
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
                Some((worktree, project))
                    if matches!(smol::block_on(self.store.chat(id)), Ok(None)) =>
                {
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
            && let Ok(projects) = smol::block_on(self.store.projects())
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
        // Nothing can be scheduled on a released workspace, so what the CLIs
        // wrote last is read here: the sessions in their queued reports, the
        // ones they were in, and the reads still under way. The sessions they
        // left before were read when they left them.
        let mut sessions = self.reading.clone();
        sessions.extend(
            self.lifecycle
                .stopping()
                .filter(|(id, ..)| !removed.contains(id))
                .filter_map(|(.., session)| session),
        );
        // A session saved by now may not have reached its chat, whose callback
        // can't run anymore, but it is the chat's last.
        let chats = self.chats.iter().map(|chat| chat.id);
        let chats: Vec<i64> = chats
            .chain(self.lifecycle.stopping().map(|(id, ..)| id))
            .chain(self.reading_last.iter().copied())
            .filter(|id| !removed.contains(id))
            .collect();
        for id in chats {
            sessions.extend(smol::block_on(self.last_session(id)));
        }
        // A chat being opened reads its last session on a thread of its own,
        // which isn't in `reading`, so it is read again here, the way
        // `open_chat` reads it.
        for id in self.lifecycle.opening().filter(|id| !removed.contains(id)) {
            let listed = smol::block_on(self.store.sessions(id)).unwrap_or_else(|err| {
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
        for chat in self.chats.iter().filter(|chat| !removed.contains(&chat.id)) {
            if chat.agent == Agent::Codex
                && let Some(terminal) = &chat.pane.terminal
                && let Some((agent_id, path)) =
                    sessions::codex_session(&terminal.terminal.read(cx).pid())
            {
                match smol::block_on(
                    self.store
                        .record_session(chat.id, chat.agent, agent_id, path, None),
                ) {
                    Ok(recorded) => sessions.push(recorded.session_id),
                    Err(err) => eprintln!("can't record the last Codex session: {err}"),
                }
            }
        }
        // The CLIs are killed on threads of their own, which the app would
        // otherwise exit before they are done with.
        let reaped: Vec<_> = self
            .chats
            .iter()
            .filter_map(|chat| chat.pane.terminal.as_ref())
            .map(|terminal| terminal.terminal.read(cx).shutdown())
            .chain(
                self.lifecycle
                    .stopping()
                    .map(|(_, reaped, _)| reaped.clone()),
            )
            .chain(deleted_reaped)
            .chain(
                self.shells
                    .values()
                    .map(|shell| shell.terminal.read(cx).shutdown()),
            )
            .chain(self.lifecycle.removal_waits().cloned())
            .collect();
        terminal::wait_reaped(reaped);
        #[cfg(target_os = "linux")]
        {
            scope::stop_all();
            scope::wait_stopped();
        }
        worktree::abandon(std::mem::take(&mut self.checkouts));
        // The worktrees of deleted chats, whose tasks won't run anymore. One
        // being removed already is waited for.
        for job in self.lifecycle.take_removals() {
            if let Some(Err(err)) = take_and_remove(&job) {
                eprintln!("can't remove a worktree on shutdown: {err:#}");
            }
        }
        for chat in self.chats.iter().filter(|chat| !removed.contains(&chat.id)) {
            if let Err(err) = import_chat(
                &self.store,
                &mut self.reports,
                chat.id,
                chat.agent,
                &chat.report,
                chat.session.into_iter().collect(),
            ) {
                eprintln!("can't import chat {} on shutdown: {err}", chat.id);
            }
        }
        sessions.sort_unstable();
        sessions.dedup();
        for id in sessions {
            if let Err(err) = smol::block_on(self.store.read_transcript(id)) {
                eprintln!("can't read session {id} on shutdown: {err}");
            }
        }
        // The sessions recorded here left others, which the store reads.
        self.store.wait_for_writes();
        self.reports.remove();
    }
}
