//! Starting a new chat, with or without a worktree of its own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agentshub_app::settings::Settings;
use agentshub_core::agent::Agent;
use agentshub_core::format::home_relative;
use agentshub_core::lifecycle::{Added, Reaped, Saved, Saving};
use agentshub_core::store::Project;
use agentshub_core::worktree::{self, Worktree};
use gpui_kit::{AppContext as _, Context, SharedString};

use crate::{Runtime, RuntimeEvent, take_and_remove};

/// When the user asked for a new chat, which what finishes later, git's
/// answers and the checkout of the chat's worktree, is checked against.
#[derive(Clone, Copy, PartialEq)]
pub struct Asked {
    navigations: u64,
    closed_all: u64,
}

/// A chat being added, with what starting it needs once it is.
#[derive(Clone)]
struct PendingChat {
    project_id: i64,
    agent: Agent,
    project: PathBuf,
    saving: Saving,
    asked: Asked,
}

impl Runtime {
    /// The agents a project can start, as last looked up for it (see
    /// `Intent::LookUpInstalled`), with its id.
    pub fn installed(&self) -> Option<(i64, &[Agent])> {
        self.installed
            .as_ref()
            .map(|(id, agents)| (*id, agents.as_slice()))
    }

    /// When the user last asked to show something, or closed all the chats,
    /// for what finishes later to check against.
    pub fn asked(&self) -> Asked {
        Asked {
            navigations: self.lifecycle.navigations(),
            closed_all: self.lifecycle.closed_all(),
        }
    }

    /// Asks for a new chat in `project`, as opening the New Chat dialog does,
    /// and looks up what the dialog offers: the agents whose CLI is installed,
    /// and whether the chat can have a worktree of its own, which it can when
    /// the project is in a git repository. The lookup gives nothing if the
    /// project was removed meanwhile, and had its id given to another.
    #[expect(
        clippy::type_complexity,
        reason = "what was looked up is handed back as it was found"
    )]
    pub fn ask_for_new_chat(
        &mut self,
        project: &Project,
        cx: &mut Context<Self>,
    ) -> (
        Asked,
        impl Future<Output = Option<anyhow::Result<(Vec<Agent>, bool)>>> + use<>,
    ) {
        let asked = self.ask_for_chat();
        let (id, path) = (project.id, project.path.clone());
        let store = self.store.clone();
        let found = cx.background_spawn({
            let path = path.clone();
            async move {
                // Only the agents whose CLI is installed can start. Chats of
                // the others stay as they are.
                let agents = Agent::installed(&path);
                let in_git = if agents.is_empty() {
                    Ok(false)
                } else {
                    worktree::repo(&path).map(|repo| repo.is_some())
                };
                (agents, in_git)
            }
        });
        let lookup = async move {
            let (agents, in_git) = found.await;
            let current = store.project_path(id).await;
            matches!(current, Ok(Some(current)) if current == path)
                .then(|| in_git.map(|in_git| (agents, in_git)))
        };
        (asked, lookup)
    }

    /// Asking for a new chat is itself a request to show something, so it
    /// supersedes the chats still opening and the new chats still on their
    /// way for earlier requests, which start in the background.
    fn ask_for_chat(&mut self) -> Asked {
        self.lifecycle.ask();
        self.asked()
    }

    /// The folder of project `id`.
    fn project_path(&self, id: i64) -> Option<PathBuf> {
        self.projects()
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.path.clone())
    }

    /// Finds the agents project `id` can start, off the foreground thread,
    /// for the buttons that start them while no chat is open.
    pub(crate) fn look_up_installed(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(path) = self.project_path(id) else {
            return;
        };
        self.installed_lookup = Some(cx.spawn(async move |this, cx| {
            let agents = cx
                .background_spawn(async move { Agent::installed(&path) })
                .await;
            this.update(cx, |this, cx| {
                this.installed = Some((id, agents));
                cx.emit(RuntimeEvent::InstalledChanged(id));
            })
            .ok();
        }));
    }

    /// Starts `agent` in a new chat of project `project_id`, as the New Chat
    /// dialog asks: titled `title`, if given, and in a new worktree of its own
    /// if `worktree` is set.
    pub(crate) fn start_new_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        title: Option<String>,
        worktree: bool,
        cx: &mut Context<Self>,
    ) {
        let asked = self.ask_for_chat();
        let Some(path) = self.project_path(project_id) else {
            return;
        };
        self.new_chat(
            project_id,
            agent,
            &path,
            worktree,
            title.unwrap_or_default(),
            asked,
            cx,
        );
    }

    /// Starts `agent` in a new chat of project `project_id` without asking
    /// anything first. With no checkbox to untick, the chat gets a worktree as
    /// the settings say.
    pub(crate) fn start_agent(&mut self, project_id: i64, agent: Agent, cx: &mut Context<Self>) {
        let Some(path) = self.project_path(project_id) else {
            return;
        };
        let asked = self.ask_for_chat();
        if !cx.global::<Settings>().worktree_by_default {
            self.new_chat(project_id, agent, &path, false, String::new(), asked, cx);
            return;
        }
        // A window that closed meanwhile doesn't start it.
        let window_closes = self.window_closes;
        cx.spawn(async move |this, cx| {
            let repo = {
                let path = path.clone();
                cx.background_spawn(async move { worktree::repo(&path) })
                    .await
            };
            this.update(cx, |this, cx| {
                if this.window_closes != window_closes {
                    return;
                }
                match repo {
                    Ok(repo) => this.new_chat(
                        project_id,
                        agent,
                        &path,
                        repo.is_some(),
                        String::new(),
                        asked,
                        cx,
                    ),
                    // Starting it in the project's folder would have it share
                    // that with other chats, which the settings asked to avoid.
                    Err(err) if this.lifecycle.closed_all() == asked.closed_all => {
                        cx.emit(RuntimeEvent::Notice(format!("{err:#}")))
                    }
                    Err(_) => {}
                }
            })
            .ok();
        })
        .detach();
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
    fn new_chat(
        &mut self,
        project_id: i64,
        agent: Agent,
        project: &Path,
        worktree: bool,
        title: String,
        asked: Asked,
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
        // Copying a large project takes seconds, with nothing else to show
        // that the chat is coming.
        let copying = SharedString::from(uuid::Uuid::new_v4().to_string());
        cx.emit(RuntimeEvent::CopyingProject {
            id: copying.clone(),
            project: project.clone(),
        });
        let window_closes = self.window_closes;
        cx.spawn(async move |this, cx| {
            // Shutdown takes it instead if the app quits first. It can't
            // between here and the chat being added, which is one step.
            let Ok(created) = created.recv().await else {
                return;
            };
            let created =
                created.map(|created| (created.worktree, unreadable_message(&created.unreadable)));
            let added = this.update(cx, |this, cx| {
                // The window closed meanwhile, so it can't show the chat.
                if this.window_closes != window_closes {
                    return false;
                }
                cx.emit(RuntimeEvent::CopiedProject(copying));
                match &created {
                    // All the chats were closed meanwhile, this one with them.
                    Ok((worktree, _)) if this.lifecycle.closed_all() != closed_all => {
                        this.remove_worktree(worktree, &project, Vec::new(), cx)
                    }
                    Err(_) if this.lifecycle.closed_all() != closed_all => {}
                    Ok((worktree, unreadable)) => {
                        if let Some(message) = unreadable {
                            cx.emit(RuntimeEvent::Notice(message.clone()));
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
                    Err(err) => cx.emit(RuntimeEvent::Notice(format!(
                        "Couldn't create a worktree: {err:#}"
                    ))),
                }
                true
            });
            // Without a window to start it in, no chat runs in the worktree.
            // The runtime outlives its window (see `tray`), so shutdown still
            // has to know of it.
            if !matches!(added, Ok(true))
                && let Ok((worktree, _)) = created
            {
                let job = this
                    .update(cx, |this, _| {
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
            this.update(cx, |this, cx| {
                // Closed to the tray, the window can't show the chat, which is
                // only listed then.
                let window = !cx.windows().is_empty();
                this.finish_adding(new, added, kept, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Starts a chat that was added for `new`, unless something since tells
    /// it not to, or it couldn't be added. `kept` says whether it was still
    /// there once added, and `window` whether there is a window to start it
    /// in.
    fn finish_adding(
        &mut self,
        new: PendingChat,
        added: Result<Option<i64>, String>,
        kept: Result<bool, String>,
        window: bool,
        cx: &mut Context<Self>,
    ) {
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
        let projects = self.projects();
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
            chat_open: id.is_some_and(|id| self.chat(id).is_some()),
            window,
            id: added,
        };
        let (added, worktree) = self.lifecycle.added(saving, saved);
        match added {
            Added::Lost => {
                if let Some((worktree, project)) = &worktree {
                    self.remove_worktree(worktree, project, Vec::new(), cx);
                }
                cx.emit(RuntimeEvent::HistoryChanged {
                    sessions: Vec::new(),
                });
            }
            Added::Failed(err) => {
                cx.emit(RuntimeEvent::Notice(format!(
                    "Couldn't add the chat: {err}"
                )));
                if let Some((worktree, project)) = &worktree {
                    self.remove_worktree(worktree, project, Vec::new(), cx);
                }
            }
            Added::Delete(id) => cx.emit(RuntimeEvent::DeletingNewChat { chat: id, worktree }),
            Added::Unknown(err) => cx.emit(RuntimeEvent::Notice(format!(
                "Couldn't start the chat: {err}"
            ))),
            Added::Listed => cx.emit(RuntimeEvent::HistoryChanged {
                sessions: Vec::new(),
            }),
            Added::Start(id) => {
                let cwd = worktree
                    .as_ref()
                    .map_or(&project, |(worktree, _)| &worktree.folder);
                // Its first session report adds its first session.
                if !self.start_chat(id, agent, cwd, None, cx) {
                    cx.emit(RuntimeEvent::DeletingNewChat { chat: id, worktree });
                } else if self.lifecycle.navigations() == asked.navigations {
                    // The sidebar doesn't list the new chat yet, so it can't
                    // find its project by itself.
                    cx.emit(RuntimeEvent::SelectProject(project_id));
                    self.activate(id, cx);
                }
                // Otherwise, as a chat opened in the background does, it only
                // shows in the sidebar as running.
                cx.emit(RuntimeEvent::HistoryChanged {
                    sessions: Vec::new(),
                });
            }
        }
    }

    /// Removes the worktree a chat ran in once `waits` say that what ran in
    /// it, its CLI, if it had one, and the shells running in it, has been
    /// reaped. Git keeps one that has changes nobody committed, which the
    /// user is told about.
    #[doc(hidden)]
    pub fn remove_worktree(
        &mut self,
        worktree: &Worktree,
        project: &Path,
        waits: Vec<Reaped>,
        cx: &mut Context<Self>,
    ) {
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
            this.update(cx, |_, cx| cx.emit(RuntimeEvent::Notice(message)))
                .ok();
        })
        .detach();
    }

    /// Stops the worktrees being checked out for new chats, as closing all
    /// the chats does. They stay listed, so shutdown still removes what they
    /// make if the app quits first.
    #[doc(hidden)]
    pub fn cancel_checkouts(&self) {
        for checkout in &self.checkouts {
            checkout.cancel();
        }
    }

    /// Stops the worktrees still being checked out, as when the app quits,
    /// and removes what they made.
    #[doc(hidden)]
    pub fn abandon_checkouts(&mut self) {
        worktree::abandon(std::mem::take(&mut self.checkouts));
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
