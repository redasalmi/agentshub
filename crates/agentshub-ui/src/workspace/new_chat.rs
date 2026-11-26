//! Starting a new chat, with or without a worktree of its own.

use super::*;

impl Workspace {
    /// Starts `agent` in a new chat of project `project_id` without asking
    /// anything first. With no checkbox to untick, the chat gets a worktree as
    /// the settings say.
    pub(super) fn start_agent(
        &mut self,
        project_id: i64,
        agent: Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .sidebar
            .read(cx)
            .projects()
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone());
        let Some(path) = path else {
            return;
        };
        let asked = self.ask_for_chat();
        if !cx.global::<Settings>().worktree_by_default {
            self.new_chat(
                project_id,
                agent,
                &path,
                false,
                String::new(),
                asked,
                window,
                cx,
            );
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let repo = {
                let path = path.clone();
                cx.background_spawn(async move { worktree::repo(&path) })
                    .await
            };
            this.update_in(cx, |this, window, cx| match repo {
                Ok(repo) => this.new_chat(
                    project_id,
                    agent,
                    &path,
                    repo.is_some(),
                    String::new(),
                    asked,
                    window,
                    cx,
                ),
                // Starting it in the project's folder would have it share
                // that with other chats, which the settings asked to avoid.
                Err(err) if this.lifecycle.closed_all() == asked.closed_all => {
                    window.push_notification(format!("{err:#}"), cx)
                }
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    /// Asking for a new chat is itself a request to show something, so it
    /// supersedes the chats still opening and the new chats still on their
    /// way for earlier requests, which start in the background.
    pub(super) fn ask_for_chat(&mut self) -> Asked {
        self.lifecycle.ask();
        self.asked()
    }

    pub(super) fn asked(&self) -> Asked {
        Asked {
            navigations: self.lifecycle.navigations(),
            closed_all: self.lifecycle.closed_all(),
        }
    }

    /// Starts an agent in a new chat of a project, in a new worktree of its
    /// own when `worktree` is set. An empty `title` leaves the chat titled
    /// after its agent's sessions. The chat starts in the background if the
    /// user asked to show something else since `asked`, and not at all if
    /// they closed all the chats.
    #[expect(
        clippy::too_many_arguments,
        reason = "GPUI's context arguments count toward the limit"
    )]
    pub(super) fn new_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        project: &Path,
        worktree: bool,
        title: String,
        asked: Asked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // All the chats were closed meanwhile, this one with them.
        if self.lifecycle.closed_all() != asked.closed_all {
            return;
        }
        if !worktree {
            self.add_chat(project_id, agent, project, None, title, asked, cx);
            return;
        }
        // Checking out the project takes a while in a large repository, and
        // the user may show another chat or the settings meanwhile. Then the
        // new chat starts in the background, so it can't take the keyboard.
        let Asked {
            navigations,
            closed_all,
        } = asked;
        let project = project.to_path_buf();
        let checkout = worktree::Checkout::start(&project);
        let created = checkout.created();
        self.checkouts.retain(|checkout| !checkout.is_taken());
        self.checkouts.push(checkout);
        cx.spawn_in(window, async move |this, cx| {
            // Shutdown takes it instead if the app quits first. It can't
            // between here and the chat being added, which is one step.
            let Ok(created) = created.recv().await else {
                return;
            };
            let created =
                created.map(|created| (created.worktree, unreadable_message(&created.unreadable)));
            let added = this.update_in(cx, |this, window, cx| match &created {
                // All the chats were closed meanwhile, this one with them.
                Ok((worktree, _)) if this.lifecycle.closed_all() != closed_all => {
                    this.remove_worktree(worktree, &project, None, cx)
                }
                Err(_) if this.lifecycle.closed_all() != closed_all => {}
                Ok((worktree, unreadable)) => {
                    if let Some(message) = unreadable {
                        window.push_notification(message.clone(), cx);
                    }
                    this.add_chat(
                        project_id,
                        agent,
                        &project,
                        Some(worktree),
                        title.clone(),
                        Asked {
                            navigations,
                            closed_all,
                        },
                        cx,
                    )
                }
                Err(err) => {
                    window.push_notification(format!("Couldn't create a worktree: {err:#}"), cx)
                }
            });
            // Without a window to start it in, no chat runs in the worktree.
            // The workspace outlives its window (see `tray`), so shutdown
            // still has to know of it.
            if added.is_err()
                && let Ok((worktree, _)) = created
            {
                let app: &mut AsyncApp = cx;
                let job = this
                    .update(app, |this, _| {
                        this.lifecycle
                            .track_removal(&worktree, &project, Vec::new())
                    })
                    .unwrap_or_else(|_| Arc::new(Mutex::new(Some((worktree, project)))));
                if let Some(Err(err)) = smol::unblock(move || take_and_remove(&job)).await {
                    eprintln!("can't remove a worktree without a window: {err:#}");
                }
            }
        })
        .detach();
    }

    /// Adds a chat and starts its agent in `project`, or in the `worktree`
    /// made for it, which goes again if the chat can't start. The chat is
    /// titled `title`, unless it's empty, and shown unless something else
    /// was asked for since `asked`, and otherwise runs in the background.
    #[expect(
        clippy::too_many_arguments,
        reason = "GPUI's context arguments count toward the limit"
    )]
    fn add_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        project: &Path,
        worktree: Option<&Worktree>,
        title: String,
        asked: Asked,
        cx: &mut Context<Self>,
    ) {
        let project = project.to_path_buf();
        let worktree = worktree.cloned();
        // The project may have been archived while the chat's worktree was
        // checked out. Adding the chat brings it back, as opening one does.
        let added =
            self.store
                .add_chat(project_id, project.clone(), agent, worktree.clone(), title);
        let saving = self.lifecycle.save(
            project_id,
            worktree.map(|worktree| (worktree, project.clone())),
        );
        let store = self.store.clone();
        let new = PendingChat {
            project_id,
            agent,
            project,
            saving,
            asked,
        };
        cx.spawn(async move |this, cx| {
            let added = added.await.map_err(|err| err.to_string());
            // It may have been deleted since, with its project. A delete
            // saved while that is read may have taken it after all, and once
            // saved, nothing else says its project was being removed.
            let kept = match added {
                Ok(Some(id)) => loop {
                    let Ok((deletions, chat)) =
                        this.read_with(cx, |this, _| (this.lifecycle.deletions(), store.chat(id)))
                    else {
                        return;
                    };
                    let kept = chat
                        .await
                        .map(|chat| chat.is_some())
                        .map_err(|err| err.to_string());
                    if this
                        .read_with(cx, |this, _| this.lifecycle.deletions())
                        .is_ok_and(|now| now != deletions)
                    {
                        continue;
                    }
                    break kept;
                },
                _ => Ok(true),
            };
            let Ok(window) = this.read_with(cx, |this, _| this.window) else {
                return;
            };
            let finished = window.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.finish_adding(new.clone(), added.clone(), kept.clone(), Some(window), cx)
                })
            });
            // Closed to the tray, the window can't show the chat, which is
            // only listed then.
            if !matches!(finished, Ok(Ok(()))) {
                this.update(cx, |this, cx| {
                    this.finish_adding(new, added, kept, None, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Starts a chat that was added for `new`, in `window`, unless something
    /// since tells it not to, or it couldn't be added. `kept` says whether it
    /// was still there once added.
    fn finish_adding(
        &mut self,
        new: PendingChat,
        added: Result<Option<i64>, String>,
        kept: Result<bool, String>,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let has_window = window.is_some();
        // Inside the window's update, the window has to say it itself.
        let mut say =
            |this: &Self, message: String, cx: &mut Context<Self>| match window.as_deref_mut() {
                Some(window) => window.push_notification(message, cx),
                None => this.say(message, cx),
            };
        let PendingChat {
            project_id,
            agent,
            project,
            saving,
            asked,
        } = new;
        let id = added.as_ref().ok().copied().flatten();
        // A listing since it was saved shows it, so it may have been opened,
        // archived or deleted from there meanwhile.
        let projects = self.catalog.projects();
        let saved = Saved {
            kept,
            closed_all: asked.closed_all,
            project_archived: projects
                .iter()
                .any(|each| each.id == project_id && each.archived_at.is_some()),
            chat_archived: projects.iter().any(|project| {
                project
                    .chats
                    .iter()
                    .any(|chat| Some(chat.id) == id && chat.archived_at.is_some())
            }),
            chat_open: self.chats.iter().any(|chat| Some(chat.id) == id),
            window: has_window,
            id: added,
        };
        let (added, worktree) = self.lifecycle.added(saving, saved);
        match added {
            Added::Lost => {
                if let Some((worktree, project)) = &worktree {
                    self.remove_worktree(worktree, project, None, cx);
                }
                self.refresh(cx);
            }
            Added::Failed(err) => {
                say(self, format!("Couldn't add the chat: {err}"), cx);
                if let Some((worktree, project)) = &worktree {
                    self.remove_worktree(worktree, project, None, cx);
                }
            }
            Added::Delete(id) => self.delete_chat(id, worktree, cx),
            Added::Unknown(err) => say(self, format!("Couldn't start the chat: {err}"), cx),
            Added::Listed => self.refresh(cx),
            Added::Start(id) => {
                let window = window.expect("a chat starts in a window");
                let cwd = worktree
                    .as_ref()
                    .map_or(&project, |(worktree, _)| &worktree.folder);
                // Its first session report adds its first session.
                if !self.start_chat(id, agent, cwd, None, window, cx) {
                    self.delete_chat(id, worktree, cx);
                } else if self.lifecycle.navigations() == asked.navigations {
                    self.activate(id, window, cx);
                    // The sidebar doesn't list the new chat yet, so it can't
                    // find its project by itself.
                    self.sidebar
                        .update(cx, |sidebar, cx| sidebar.select(project_id, cx));
                } else {
                    // As a chat opened in the background does, it only shows
                    // in the sidebar as running.
                    self.sync_sidebar(cx);
                    cx.notify();
                }
                self.refresh(cx);
            }
        }
    }

    /// Removes the worktree a chat ran in once its CLI, if it had one, and
    /// the shells running in it have been reaped. Git keeps one that has
    /// changes nobody committed, which the user is told about.
    pub(super) fn remove_worktree(
        &mut self,
        worktree: &Worktree,
        project: &Path,
        reaped: Option<smol::channel::Receiver<()>>,
        cx: &mut Context<Self>,
    ) {
        let inside: Vec<PathBuf> = self
            .shells
            .keys()
            .filter(|cwd| cwd.starts_with(&worktree.root))
            .cloned()
            .collect();
        let mut waits: Vec<_> = reaped.into_iter().collect();
        for cwd in &inside {
            if let Some(shell) = self.shells.remove(cwd) {
                waits.push(shell.terminal.read(cx).shutdown());
            }
        }
        if !inside.is_empty() {
            self.sync_terminals(cx);
            cx.notify();
        }
        let folder = worktree.root.clone();
        let job = self
            .lifecycle
            .track_removal(worktree, project, waits.clone());
        cx.spawn(async move |this, cx| {
            // What the CLI and the shells write as they exit, like a shell's
            // history, would otherwise go to a folder being deleted.
            for reaped in waits {
                reaped.recv().await.ok();
            }
            // Shutdown removed it if the app quit first.
            let Some(removed) = smol::unblock(move || take_and_remove(&job)).await else {
                return;
            };
            let message = match removed {
                Ok(worktree::Removal::Removed) => return,
                Ok(worktree::Removal::Rescued(branch)) => format!(
                    "Removed the worktree at {}. Commits made there on a detached HEAD are \
                     kept on the branch {branch}.",
                    home_relative(&folder)
                ),
                Ok(worktree::Removal::Kept(why)) => format!(
                    "Kept the worktree at {}: {}",
                    home_relative(&folder),
                    why.trim_start_matches("fatal: ")
                ),
                Err(err) => {
                    eprintln!("can't remove the worktree {}: {err:#}", folder.display());
                    format!(
                        "Couldn't remove the worktree at {}: {err:#}",
                        home_relative(&folder)
                    )
                }
            };
            this.update(cx, |this, cx| this.say(message, cx)).ok();
        })
        .detach();
    }
}

/// Tells what of the project the worktree lacks as it couldn't be read, if
/// anything.
fn unreadable_message(unreadable: &[PathBuf]) -> Option<String> {
    const SHOWN: usize = 3;
    let mut names: Vec<String> = unreadable
        .iter()
        .take(SHOWN)
        .map(|path| path.display().to_string())
        .collect();
    if unreadable.len() > SHOWN {
        names.push(format!("{} more", unreadable.len() - SHOWN));
    }
    let last = names.pop()?;
    let list = if names.is_empty() {
        last
    } else {
        format!("{} and {last}", names.join(", "))
    };
    Some(format!(
        "The worktree doesn't have what you can't read in the project: {list}."
    ))
}
