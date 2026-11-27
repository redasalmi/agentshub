//! Closing, deleting and archiving chats and projects.

use super::*;

impl Workspace {
    /// Closes a running chat, asking first if its agent is in the middle of
    /// something. Opening the chat again resumes its session.
    pub(super) fn confirm_close_chat(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let working = self
            .runtime
            .read(cx)
            .chat(id)
            .is_some_and(|chat| chat.status == Some(Status::Working));
        if !working {
            self.close_chats(vec![id], window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Close this chat?")
                .description(
                    "Its agent is working, and closing the chat stops it mid-turn. \
                     Opening the chat again resumes its session.",
                )
                .ok_text("Close")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.close_chats(vec![id], window, cx))
                        .ok();
                    true
                })
        });
    }

    pub(super) fn confirm_delete_chat(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The sidebar doesn't list a chat just added yet.
        let listed = self.runtime.read(cx).saved_projects();
        let asked = self.runtime.read(cx).asked();
        cx.spawn_in(window, async move |this, cx| {
            // The worktree Agents Hub made for the chat, and its project's
            // folder.
            let worktree = listed
                .await
                .inspect_err(|err| eprintln!("can't list the projects: {err}"))
                .ok()
                .and_then(|projects| {
                    projects.into_iter().find_map(|project| {
                        let chat = project.chats.into_iter().find(|chat| chat.id == id)?;
                        Some((chat.worktree?, project.path))
                    })
                });
            let description = match worktree {
                Some(_) => {
                    "Its agent is stopped, and the chat is deleted from Agents Hub. \
                     The agent's own session files stay. Its worktree is removed unless it \
                     has changes nobody committed, and its branch stays."
                }
                None => {
                    "Its agent is stopped, and the chat is deleted from Agents Hub. \
                     The agent's own session files stay."
                }
            };
            this.update_in(cx, |this, window, cx| {
                // The user may have shown something else meanwhile, or opened
                // another dialog, which the question would be out of place in.
                if this.runtime.read(cx).asked() != asked || window.has_active_dialog(cx) {
                    return;
                }
                let this = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let (this, worktree) = (this.clone(), worktree.clone());
                    alert
                        .confirm()
                        .title("Delete this chat?")
                        .description(description)
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .on_ok(move |_, _, cx| {
                            let worktree = worktree.clone();
                            this.update(cx, |this, cx| {
                                this.apply(Intent::DeleteChat { chat: id, worktree }, cx)
                            })
                            .ok();
                            true
                        })
                });
            })
            .ok();
        })
        .detach();
    }

    /// Archives a chat, closing it first, and asks first if its agent is in
    /// the middle of something.
    pub(super) fn confirm_archive_chat(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let working = self
            .runtime
            .read(cx)
            .chat(id)
            .is_some_and(|chat| chat.status == Some(Status::Working));
        if !working {
            self.archive_chat(id, window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Archive this chat?")
                .description(
                    "Its agent is working, and archiving the chat stops it mid-turn. \
                     Opening the chat again resumes its session.",
                )
                .ok_text("Archive")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.archive_chat(id, window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Closes a chat and archives it. Opening it again brings it back and
    /// resumes its session.
    pub(super) fn archive_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_closing(Intent::ArchiveChat(id), window, cx);
    }

    /// Archives a project, closing its chats, and asks first if any of their
    /// agents is in the middle of something.
    pub(super) fn confirm_archive_project(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self.runtime.read(cx).chats_of(id);
        let working = self
            .runtime
            .read(cx)
            .open_chats()
            .any(|chat| ids.contains(&chat.id) && chat.status == Some(Status::Working));
        if !working {
            self.archive_project(id, window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Archive this project?")
                .description(
                    "An agent in it is working, and archiving the project stops it mid-turn. \
                     Opening one of its chats again brings the project back and resumes the \
                     chat's session.",
                )
                .ok_text("Archive")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.archive_project(id, window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Closes a project's chats and its shell, and archives it with them.
    /// Opening one of its chats again brings it back.
    pub(super) fn archive_project(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .sidebar
            .read(cx)
            .projects()
            .iter()
            .any(|project| project.id == id)
        {
            return;
        }
        self.apply_closing(Intent::ArchiveProject(id), window, cx);
    }

    /// Shows `change` at once, and saves it. The projects are listed as saved
    /// once every change shown is, which takes back one that couldn't be.
    pub(super) fn change(&mut self, change: Change, cx: &mut Context<Self>) {
        self.apply(Intent::Change(change), cx);
    }

    /// Asks the runtime for what `intent` says.
    pub(super) fn apply(&self, intent: Intent, cx: &mut App) {
        self.runtime
            .update(cx, |runtime, cx| runtime.apply(intent, cx));
    }
}
