//! The window's side of starting new chats: the agents offered, and the
//! notice while a project is copied into a new worktree.

use super::*;

impl Workspace {
    /// Offers the agents project `id` can start, now that they have been
    /// looked up, in the search box if it was opened for that project.
    pub(super) fn installed_changed(&mut self, id: i64, cx: &mut Context<Self>) {
        let entries = self
            .runtime
            .read(cx)
            .installed()
            .filter(|(installed, _)| *installed == id)
            .map(|(_, agents)| start_entries(id, agents));
        if let Some((project, sections)) = &self.palette_start
            && *project == id
            && let Some(entries) = entries
        {
            sections.set(PALETTE_START, entries, cx);
        }
        cx.notify();
    }

    /// Says `project` is being copied into a new worktree, until
    /// `copied_project` with the same `id`.
    pub(super) fn copying_project(&self, id: SharedString, project: &Path, cx: &mut Context<Self>) {
        let message = format!(
            "Copying {} into a new worktree for the chat…",
            project.file_name().map_or_else(
                || project.display().to_string(),
                |name| { name.to_string_lossy().into_owned() }
            )
        );
        cx.with_window(cx.entity_id(), |window, cx| {
            window.push_notification(
                Notification::new()
                    .id1::<CopyingProject>(id)
                    .message(message)
                    .autohide(false),
                cx,
            )
        });
    }

    pub(super) fn copied_project(&self, id: SharedString, cx: &mut Context<Self>) {
        cx.with_window(cx.entity_id(), |window, cx| {
            window.remove_notification1::<CopyingProject>(id, cx)
        });
    }
}

/// Tells apart the notice that a project is being copied into a worktree.
struct CopyingProject;
