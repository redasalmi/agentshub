//! Adding, removing and archiving projects, and the database file behind them.

use super::*;

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
            this.update(cx, |this, cx| this.add_folders(&[path], cx))
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
        self.add_folders(&folders, cx);
    }

    /// Adds folders as projects, in the order given at the top of the list,
    /// and selects the first. A folder that already is a project is selected
    /// where it is.
    fn add_folders(&mut self, folders: &[PathBuf], cx: &mut Context<Self>) {
        let folders = folders.to_vec();
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
                        this.update(cx, |this, cx| this.say(message, cx)).ok();
                    }
                }
            }
            let Some(id) = first else {
                return;
            };
            // Selecting it looks up what can start in it, so it is listed
            // first.
            this.update(cx, |this, cx| {
                this.select_listed = Some(id);
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
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
            let Ok(exported) = this.update(cx, |this, _| this.store.export(path.clone())) else {
                return;
            };
            let message = match exported.await {
                Ok(()) => format!("Exported the database to {}", path.display()),
                Err(err) => format!("Couldn't export the database: {err}"),
            };
            this.update(cx, |this, cx| this.say(message, cx)).ok();
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
            let Some(path) = path else {
                this.update(cx, |this, _| this.importing = false).ok();
                return;
            };
            // Checking and copying a large database takes a while.
            let prepared = smol::unblock({
                let path = path.clone();
                move || store::prepare_import(&path)
            })
            .await;
            // Another import can start once this one is shown: the dialog is
            // modal, and each check has a copy of its own. The window may have
            // closed to the tray meanwhile, which the workspace outlives, so
            // this doesn't wait for it.
            this.update(cx, |this, _| this.importing = false).ok();
            let unshown = prepared.as_ref().ok().map(|(pending, _)| pending.clone());
            let shown = this.update_in(cx, |this, window, cx| match prepared {
                Ok((pending, projects)) => {
                    this.confirm_import(&path, pending, projects, window, cx)
                }
                Err(err) => window.push_notification(
                    format!("Couldn't import {}: {err:#}", home_relative(&path)),
                    cx,
                ),
            });
            // Without a window to confirm it in, the copy is turned down.
            if shown.is_err()
                && let Some(pending) = unshown
            {
                pending.discard();
            }
        })
        .detach();
    }

    /// Shows what a checked database holds, and restarts with it in place of
    /// the database once the user confirms.
    fn confirm_import(
        &mut self,
        path: &Path,
        pending: store::PendingImport,
        projects: Vec<store::Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let chats: usize = projects.iter().map(|project| project.chats.len()).sum();
        let holds = match (projects.len(), chats) {
            (0, _) => format!("{name} has no projects."),
            (projects, chats) => format!(
                "{name} has {} and {}:",
                plural(projects, "project"),
                plural(chats, "chat")
            ),
        };
        let dir = store::path()
            .and_then(|path| path.parent().map(home_relative))
            .unwrap_or_default();
        let projects: Rc<[store::Project]> = projects.into();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let theme = cx.theme();
            let (confirm, discard) = (pending.clone(), pending.clone());
            alert
                .confirm()
                .width(px(520.))
                .title("Replace your chats with these?")
                .description(holds.clone())
                .when(!projects.is_empty(), |alert| {
                    alert.child(render_import(&projects, theme))
                })
                .child(
                    div()
                        .text_sm()
                        .line_height(rems(1.4))
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "Agents Hub restarts to use them, which stops the agents running \
                             now. Your projects, chats and settings are replaced; the database \
                             holding them is backed up first in {dir}, and importing that \
                             backup brings them back."
                        )),
                )
                .ok_text("Import and restart")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    match confirm.confirm() {
                        Ok(()) => cx.restart(),
                        Err(err) => window.push_notification(
                            format!("Couldn't import the database: {err:#}"),
                            cx,
                        ),
                    }
                    true
                })
                // The checked copy is gone once confirmed, so this only
                // deletes one the user turned down.
                .on_close(move |_, _, _| discard.discard())
        });
    }

    pub(super) fn confirm_remove_project(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The sidebar doesn't list a chat just added yet.
        let listed = self.store.projects();
        let asked = self.asked();
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
                if this.asked() != asked || window.has_active_dialog(cx) {
                    return;
                }
                let this = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let this = this.clone();
                    alert
                        .confirm()
                        .title("Remove this project?")
                        .description(description)
                        .ok_text("Remove")
                        .ok_variant(ButtonVariant::Danger)
                        .on_ok(move |_, _, cx| {
                            this.update(cx, |this, cx| this.remove_project(id, cx)).ok();
                            true
                        })
                });
            })
            .ok();
        })
        .detach();
    }

    /// Stops a project's agents and shell, and removes it with its chats.
    /// The worktrees of its chats go once that is saved, and until then, its
    /// chats can't be opened, nor a chat added to it start.
    fn remove_project(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(path) = self
            .sidebar
            .read(cx)
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
        // being added, which the sidebar doesn't list yet.
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
        // Dropping its terminal kills the shell.
        self.shells.remove(&project.path);
        self.sync_terminals(cx);
        let removed = self.store.remove_project(id, project.path.clone());
        cx.spawn(async move |this, cx| {
            let removed = removed.await;
            this.update(cx, |this, cx| {
                this.lifecycle.project_removed(id);
                if let Err(err) = &removed {
                    eprintln!("can't remove project {id}: {err:#}");
                    this.say(format!("Couldn't remove the project: {err}"), cx);
                }
                this.finish_deleting(&chats, removed.is_ok(), cx);
            })
            .ok();
        })
        .detach();
    }
}
