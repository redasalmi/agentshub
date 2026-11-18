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
            .chats
            .iter()
            .any(|chat| chat.id == id && chat.status == Some(Status::Working));
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
        let listed = self.store.projects();
        let asked = self.asked();
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
                if this.asked() != asked || window.has_active_dialog(cx) {
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
                            this.update(cx, |this, cx| this.delete_chat(id, worktree.clone(), cx))
                                .ok();
                            true
                        })
                });
            })
            .ok();
        })
        .detach();
    }

    /// Stops a chat's agent and deletes the chat. Its worktree goes once that
    /// is saved, and until then, the chat can't be opened.
    pub(super) fn delete_chat(
        &mut self,
        id: i64,
        worktree: Option<(Worktree, PathBuf)>,
        cx: &mut Context<Self>,
    ) {
        self.lifecycle.cancel_opening(id);
        let reaped = self
            .stop_chat(id, cx)
            .or_else(|| self.lifecycle.reaping(id));
        self.lifecycle.mark_deleting(id, worktree, reaped);
        let deleted = self.store.delete_chat(id);
        cx.spawn(async move |this, cx| {
            let deleted = deleted.await;
            this.update(cx, |this, cx| {
                if let Err(err) = &deleted {
                    eprintln!("can't delete chat {id}: {err:#}");
                    this.say(format!("Couldn't delete the chat: {err}"), cx);
                }
                this.finish_deleting(&[id], deleted.is_ok(), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Lets `chats` be opened again, once deleting them is done, and if they
    /// were deleted, removes their worktrees.
    pub(super) fn finish_deleting(&mut self, chats: &[i64], deleted: bool, cx: &mut Context<Self>) {
        for (id, Deleted { worktree, reaped }) in self.lifecycle.finish_deleting_all(chats, deleted)
        {
            // Its notification can't open it anymore.
            alerts::withdraw(id, cx);
            if let Some((worktree, project)) = &worktree {
                self.remove_worktree(worktree, project, reaped, cx);
            }
        }
        self.refresh(cx);
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
            .chats
            .iter()
            .any(|chat| chat.id == id && chat.status == Some(Status::Working));
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
        self.lifecycle.cancel_opening(id);
        self.close_chats(vec![id], window, cx);
        self.change(
            Change::ArchiveChat {
                chat: id,
                archived: true,
            },
            cx,
        );
        alerts::withdraw(id, cx);
        self.offer_undo(
            "Archived the chat.",
            Change::ArchiveChat {
                chat: id,
                archived: false,
            },
            window,
            cx,
        );
    }

    /// Archives a project, closing its chats, and asks first if any of their
    /// agents is in the middle of something.
    pub(super) fn confirm_archive_project(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self.chats_of(id, cx);
        let working = self
            .chats
            .iter()
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
        let Some((path, worktrees)) = self
            .sidebar
            .read(cx)
            .projects()
            .iter()
            .find(|project| project.id == id)
            .map(|project| {
                let worktrees: Vec<PathBuf> = project
                    .chats
                    .iter()
                    .filter_map(|chat| Some(chat.worktree.as_ref()?.root.clone()))
                    .collect();
                (project.path.clone(), worktrees)
            })
        else {
            return;
        };
        let ids = self.chats_of(id, cx);
        for id in &ids {
            self.lifecycle.cancel_opening(*id);
        }
        self.close_chats(ids.clone(), window, cx);
        // Dropping their terminals kills the shells, as removing the project
        // does: its own, and the ones in its chats' worktrees.
        let shells = self.shells.len();
        self.shells
            .retain(|cwd, _| *cwd != path && !worktrees.iter().any(|root| cwd.starts_with(root)));
        if self.shells.len() != shells {
            self.sync_terminals(cx);
        }
        self.lifecycle.archive_project();
        self.change(
            Change::ArchiveProject {
                project: id,
                archived: true,
            },
            cx,
        );
        for id in ids {
            alerts::withdraw(id, cx);
        }
        self.offer_undo(
            "Archived the project.",
            Change::ArchiveProject {
                project: id,
                archived: false,
            },
            window,
            cx,
        );
    }

    /// The ids of a project's chats.
    pub(super) fn chats_of(&self, project_id: i64, cx: &App) -> Vec<i64> {
        self.sidebar
            .read(cx)
            .projects()
            .iter()
            .filter(|project| project.id == project_id)
            .flat_map(|project| &project.chats)
            .map(|chat| chat.id)
            .collect()
    }

    /// Shows `change` at once, and saves it. The projects are listed as saved
    /// once every change shown is, which takes back one that couldn't be.
    pub(super) fn change(&mut self, change: Change, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.change(&change, cx));
        self.unsaved += 1;
        let saved = self.store.change(change);
        cx.spawn(async move |this, cx| {
            let saved = saved.await;
            this.update(cx, |this, cx| {
                this.unsaved -= 1;
                if let Err(err) = saved {
                    eprintln!("can't save a change: {err:#}");
                    this.say(
                        format!("Couldn't save the change, so it is undone: {err}"),
                        cx,
                    );
                }
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }
}
