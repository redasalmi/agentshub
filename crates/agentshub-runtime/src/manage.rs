//! Managing the projects and chats: adding, removing, archiving and closing
//! them, the database file behind them, and the settings.

use std::fs;
use std::path::PathBuf;

use gpui_kit::{AppContext as _, Context};

use agentshub_app::alerts;
use agentshub_app::settings::Value;
use agentshub_core::lifecycle::Deleted;
use agentshub_core::status::Status;
use agentshub_core::store::{self, Change};
use agentshub_core::worktree::Worktree;

use crate::{ChatState, IdleCheck, Intent, Runtime, RuntimeEvent};

/// The chats among `states` whose CLI runs at rest with nothing unseen,
/// which closing the idle chats closes if stopping them loses nothing else.
pub(crate) fn pick_idle(states: &[ChatState]) -> Vec<i64> {
    states
        .iter()
        .filter(|chat| {
            chat.running && !chat.unseen && matches!(chat.status, Some(Status::Idle | Status::Done))
        })
        .map(|chat| chat.id)
        .collect()
}

impl Runtime {
    /// Adds folders as projects, in the order given at the top of the list,
    /// and selects the first. A folder that already is a project is selected
    /// where it is.
    pub(crate) fn add_folders(&self, folders: Vec<PathBuf>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // Each one goes on top, so the last goes in first.
            let folders = cx
                .background_spawn(async move {
                    folders
                        .into_iter()
                        .rev()
                        .map(|path| fs::canonicalize(&path).unwrap_or(path))
                        .collect::<Vec<_>>()
                })
                .await;
            let Ok(added) = this.update(cx, |this, _| {
                folders
                    .into_iter()
                    .map(|path| (path.clone(), this.store.add_project(path)))
                    .collect::<Vec<_>>()
            }) else {
                return;
            };
            let mut first = None;
            for (path, added) in added {
                match added.await {
                    Ok(id) => first = Some(id),
                    Err(err) => {
                        let message = format!("Couldn't add {}: {err}", path.display());
                        this.update(cx, |_, cx| cx.emit(RuntimeEvent::Notice(message)))
                            .ok();
                    }
                }
            }
            let Some(id) = first else {
                return;
            };
            // Selecting it looks up what can start in it, so it is listed
            // first.
            this.update(cx, |this, cx| {
                this.catalog.select_when_listed(id);
                cx.emit(RuntimeEvent::HistoryChanged {
                    sessions: Vec::new(),
                });
            })
            .ok();
        })
        .detach();
    }

    /// Copies the database to `path`, so the history can live outside the
    /// one file agentshub reads.
    pub(crate) fn export(&self, path: PathBuf, cx: &mut Context<Self>) {
        let exported = self.store.export(path.clone());
        cx.spawn(async move |this, cx| {
            let message = match exported.await {
                Ok(()) => format!("Exported the database to {}", path.display()),
                Err(err) => format!("Couldn't export the database: {err}"),
            };
            this.update(cx, |_, cx| cx.emit(RuntimeEvent::Notice(message)))
                .ok();
        })
        .detach();
    }

    /// Checks the database at `path` and copies it, for the window to show
    /// what it holds before it replaces this one (see
    /// `RuntimeEvent::ImportChecked`).
    pub(crate) fn check_import(&self, path: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // Checking and copying a large database takes a while.
            let checked = smol::unblock({
                let path = path.clone();
                move || store::prepare_import(&path)
            })
            .await;
            let unshown = checked.as_ref().ok().map(|(pending, _)| pending.clone());
            let told = this.update(cx, |_, cx| {
                cx.emit(RuntimeEvent::ImportChecked { path, checked })
            });
            if told.is_err()
                && let Some(pending) = unshown
            {
                pending.discard();
            }
        })
        .detach();
    }

    /// Puts a checked database in place of this one, and has the app restart
    /// to use it.
    pub(crate) fn confirm_import(&self, pending: &store::PendingImport, cx: &mut Context<Self>) {
        match pending.confirm() {
            Ok(()) => cx.emit(RuntimeEvent::Restart),
            Err(err) => cx.emit(RuntimeEvent::Notice(format!(
                "Couldn't import the database: {err:#}"
            ))),
        }
    }

    /// Saves settings the window applied, in order. If one can't be saved,
    /// the user is told it lasts until Agents Hub quits, and the ones after it
    /// aren't waited for.
    pub(crate) fn save_settings(
        &self,
        settings: Vec<(&'static str, Value)>,
        cx: &mut Context<Self>,
    ) {
        let saved: Vec<_> = settings
            .into_iter()
            .map(|(key, value)| value.save(key, &self.store))
            .collect();
        cx.spawn(async move |this, cx| {
            for saved in saved {
                if let Err(err) = saved.await {
                    eprintln!("can't save the settings: {err:#}");
                    let message = format!(
                        "Couldn't save the setting, so it lasts until Agents Hub quits: {err}"
                    );
                    this.update(cx, |_, cx| cx.emit(RuntimeEvent::Notice(message)))
                        .ok();
                    return;
                }
            }
        })
        .detach();
    }

    /// The ids of a project's chats.
    pub fn chats_of(&self, project: i64) -> Vec<i64> {
        self.projects()
            .iter()
            .filter(|each| each.id == project)
            .flat_map(|each| &each.chats)
            .map(|chat| chat.id)
            .collect()
    }

    /// Closes chats, as `Intent::Suspend` closes each.
    pub(crate) fn close_chats(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) {
        for id in ids {
            self.suspend(id, cx);
        }
    }

    /// Closes the chats `check` found idle (see `idle_chats`).
    pub(crate) fn close_idle(&mut self, check: IdleCheck, cx: &mut Context<Self>) {
        let ids = self.idle_chats(check, cx);
        self.close_chats(ids, cx);
    }

    /// Closes every open chat, stopping its CLI even mid-turn, and the ones
    /// still opening or waiting for their worktree.
    pub(crate) fn close_all(&mut self, cx: &mut Context<Self>) {
        // A chat waiting for its last session to be read, or for its stopped
        // CLI to be reaped, would start once that's done, and so would a new
        // chat once its worktree is checked out. The checkouts stay listed, so
        // shutdown still removes what they make if the app quits first.
        self.lifecycle.close_all();
        self.cancel_checkouts();
        let ids = self.chats.iter().map(|chat| chat.id).collect();
        self.close_chats(ids, cx);
    }

    /// Closes a chat and archives it. Opening it again brings it back and
    /// resumes its session.
    pub(crate) fn archive_chat(&mut self, id: i64, cx: &mut Context<Self>) {
        self.lifecycle.cancel_opening(id);
        self.close_chats(vec![id], cx);
        self.change(
            Change::ArchiveChat {
                chat: id,
                archived: true,
            },
            cx,
        );
        alerts::withdraw(id, cx);
        cx.emit(RuntimeEvent::Undoable {
            message: "Archived the chat.".into(),
            undo: Intent::Change(Change::ArchiveChat {
                chat: id,
                archived: false,
            }),
        });
    }

    /// Closes a project's chats, and archives it with them. Opening one of
    /// its chats again brings it back.
    pub(crate) fn archive_project(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.projects().iter().any(|project| project.id == id) {
            return;
        }
        let ids = self.chats_of(id);
        for id in &ids {
            self.lifecycle.cancel_opening(*id);
        }
        self.close_chats(ids.clone(), cx);
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
        cx.emit(RuntimeEvent::Undoable {
            message: "Archived the project.".into(),
            undo: Intent::Change(Change::ArchiveProject {
                project: id,
                archived: false,
            }),
        });
    }

    /// Stops a chat's agent and deletes the chat, with `worktree`, the one
    /// Agents Hub made for it in its project's folder. The worktree goes once
    /// that is saved, and until then, the chat can't be opened.
    pub(crate) fn delete_chat(
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
                    cx.emit(RuntimeEvent::Notice(format!(
                        "Couldn't delete the chat: {err}"
                    )));
                }
                this.finish_deleting(&[id], deleted.is_ok(), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Lets `chats` be opened again, once deleting them is done, and if they
    /// were deleted, has their worktrees removed.
    fn finish_deleting(&mut self, chats: &[i64], deleted: bool, cx: &mut Context<Self>) {
        for (id, Deleted { worktree, reaped }) in self.lifecycle.finish_deleting_all(chats, deleted)
        {
            // Its notification can't open it anymore.
            alerts::withdraw(id, cx);
            if let Some((worktree, project)) = worktree {
                cx.emit(RuntimeEvent::RemovingWorktree {
                    worktree,
                    project,
                    reaped,
                });
            }
        }
        cx.emit(RuntimeEvent::HistoryChanged {
            sessions: Vec::new(),
        });
    }

    /// Stops a project's agents, and removes it with its chats. The worktrees
    /// of its chats go once that is saved, and until then, its chats can't be
    /// opened, nor a chat added to it start.
    pub(crate) fn remove_project(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(path) = self
            .projects()
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.path.clone())
        else {
            return;
        };
        if !self.lifecycle.remove_project(id, path.clone()) {
            return;
        }
        // The listing has the chats asked for before it, including one still
        // being added, which the catalog doesn't list yet.
        let listed = self.store.projects();
        cx.spawn(async move |this, cx| {
            let project = listed
                .await
                .inspect_err(|err| eprintln!("can't list the projects: {err}"))
                .unwrap_or_default()
                .into_iter()
                .find(|project| project.id == id && project.path == path);
            this.update(cx, |this, cx| match project {
                Some(project) => this.remove_listed(project, cx),
                None => this.lifecycle.project_removed(id),
            })
            .ok();
        })
        .detach();
    }

    fn remove_listed(&mut self, project: store::Project, cx: &mut Context<Self>) {
        let id = project.id;
        let chats: Vec<i64> = project.chats.iter().map(|chat| chat.id).collect();
        for chat in project.chats {
            self.lifecycle.cancel_opening(chat.id);
            let reaped = self
                .stop_chat(chat.id, cx)
                .or_else(|| self.lifecycle.reaping(chat.id));
            let worktree = chat
                .worktree
                .map(|worktree| (worktree, project.path.clone()));
            self.lifecycle.mark_deleting(chat.id, worktree, reaped);
        }
        cx.emit(RuntimeEvent::RemovingProject(project.path.clone()));
        let removed = self.store.remove_project(id, project.path);
        cx.spawn(async move |this, cx| {
            let removed = removed.await;
            this.update(cx, |this, cx| {
                this.lifecycle.project_removed(id);
                if let Err(err) = &removed {
                    eprintln!("can't remove project {id}: {err:#}");
                    cx.emit(RuntimeEvent::Notice(format!(
                        "Couldn't remove the project: {err}"
                    )));
                }
                this.finish_deleting(&chats, removed.is_ok(), cx);
            })
            .ok();
        })
        .detach();
    }
}
