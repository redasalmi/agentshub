//! Adding, removing and archiving projects, and the database file behind them.

use super::*;
use agentshub_ui_kit::dialogs::{self, Confirm, Import};

impl Workspace {
    pub(super) fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a folder: {err}"), cx)
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            this.update(cx, |this, cx| {
                this.apply(Intent::AddFolders(vec![path]), cx)
            })
            .ok();
        })
        .detach();
    }

    /// Adds the folders among paths dropped on the sidebar as projects, and
    /// says so when none of them is a folder.
    pub(super) fn add_dropped(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folders: Vec<PathBuf> = paths.iter().filter(|path| path.is_dir()).cloned().collect();
        if folders.is_empty() {
            window.push_notification("Drop a folder to add it as a project", cx);
            return;
        }
        self.apply(Intent::AddFolders(folders), cx);
    }

    /// Copies the database where the user asks, so the history can live
    /// outside the one file agentshub reads.
    pub(super) fn export_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let name = format!("agentshub-{}.db", jiff::Zoned::now().date());
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a file: {err}"), cx)
                    })
                    .ok();
                    return;
                }
                _ => return,
            };
            this.update(cx, |this, cx| this.apply(Intent::Export(path), cx))
                .ok();
        })
        .detach();
    }

    /// Replaces the database with one the user picks, once it is checked and
    /// they have seen what it holds. Agents Hub restarts to use it, which
    /// stops the agents as quitting does, and backs up the database it
    /// replaces on the way back up.
    pub(super) fn import_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // One pick and check at a time, so a second click doesn't open a
        // second picker, or a second dialog over the first.
        if self.importing {
            return;
        }
        self.importing = true;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(format!("Couldn't pick a file: {err}"), cx)
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            this.update(cx, |this, cx| match path {
                Some(path) => this.apply(Intent::Import(path), cx),
                None => this.importing = false,
            })
            .ok();
        })
        .detach();
    }

    /// Shows what the database checked at `path` holds, or why it can't be
    /// imported.
    pub(super) fn import_checked(
        &mut self,
        path: &Path,
        checked: &Result<(store::PendingImport, Vec<store::Project>)>,
        cx: &mut Context<Self>,
    ) {
        // Another import can start once this one is shown: the dialog is
        // modal, and each check has a copy of its own. The window may have
        // closed to the tray meanwhile, which the workspace outlives, so this
        // doesn't wait for it.
        self.importing = false;
        let shown = cx.with_window(cx.entity_id(), |window, cx| match checked {
            Ok((pending, projects)) => {
                self.confirm_import(path, pending.clone(), projects.clone(), window, cx)
            }
            Err(err) => window.push_notification(
                format!("Couldn't import {}: {err:#}", home_relative(path)),
                cx,
            ),
        });
        // Without a window to confirm it in, the copy is turned down.
        if shown.is_none()
            && let Ok((pending, _)) = checked
        {
            pending.discard();
        }
    }

    /// Shows what a checked database holds, and restarts with it in place of
    /// the database once the user confirms.
    fn confirm_import(
        &self,
        path: &Path,
        pending: store::PendingImport,
        projects: Vec<store::Project>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let backup_dir = store::path()
            .and_then(|path| path.parent().map(home_relative))
            .unwrap_or_default();
        let runtime = self.runtime.downgrade();
        let discard = pending.clone();
        dialogs::import(
            window,
            cx,
            Import {
                path: path.to_path_buf(),
                projects,
                backup_dir,
            },
            move |_, cx| {
                let confirm = Intent::ConfirmImport(pending.clone());
                runtime
                    .update(cx, |runtime, cx| runtime.apply(confirm, cx))
                    .ok();
            },
            // The checked copy is gone once confirmed, so this only deletes
            // one the user turned down.
            move |_, _| discard.discard(),
        );
    }

    pub(super) fn confirm_remove_project(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The sidebar doesn't list a chat just added yet.
        let listed = self.runtime.read(cx).saved_projects();
        let asked = self.runtime.read(cx).asked();
        cx.spawn_in(window, async move |this, cx| {
            let has_worktrees = listed
                .await
                .inspect_err(|err| eprintln!("can't list the projects: {err}"))
                .unwrap_or_default()
                .into_iter()
                .filter(|project| project.id == id)
                .flat_map(|project| project.chats)
                .any(|chat| chat.worktree.is_some());
            let description = if has_worktrees {
                "Its agents are stopped, and its chats are deleted from Agents Hub. \
                 The folder and the agents' own session files stay. The worktrees of its chats \
                 are removed unless they have changes nobody committed, and their branches stay."
            } else {
                "Its agents are stopped, and its chats are deleted from Agents Hub. \
                 The folder and the agents' own session files stay."
            };
            this.update_in(cx, |this, window, cx| {
                // The user may have shown something else meanwhile, or opened
                // another dialog, which the question would be out of place in.
                if this.runtime.read(cx).asked() != asked || window.has_active_dialog(cx) {
                    return;
                }
                let this = cx.entity().downgrade();
                dialogs::confirm(
                    window,
                    cx,
                    Confirm {
                        title: "Remove this project?".into(),
                        description: description.into(),
                        ok_text: "Remove".into(),
                        danger: true,
                    },
                    move |_, cx| {
                        this.update(cx, |this, cx| this.apply(Intent::RemoveProject(id), cx))
                            .ok();
                    },
                );
            })
            .ok();
        })
        .detach();
    }
}
