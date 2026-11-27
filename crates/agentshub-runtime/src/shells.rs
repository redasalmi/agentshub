//! The projects' shells, which run beside their chats, the terminals that
//! build frames, and opening a chat's folder in a code editor.

use std::path::{Path, PathBuf};

use gpui_kit::{App, AppContext as _, Context, Entity, Subscription};

use agentshub_app::editors::Installed;
use agentshub_app::settings::Settings;
use agentshub_core::editors::{self, Editor};
use agentshub_core::lifecycle::Reaped;
use agentshub_core::store::Project;
use agentshub_core::worktree::Worktree;

use crate::terminal::{self, Exited, Pty, Terminal};
use crate::{Runtime, RuntimeEvent};

/// A project's shell, for quick commands next to its chats. Nothing about it
/// is saved, and it runs until it exits or the app quits.
pub(crate) struct Shell {
    pub(crate) terminal: Entity<Terminal>,
    /// Whether the panel shows this shell. Each project keeps its own, so
    /// switching projects leaves the panel as it was in each of them.
    shown: bool,
    _exited: Subscription,
}

impl Runtime {
    /// The shell of the folder the panel is for, if one runs there: its
    /// folder, its terminal, and whether the panel shows it. The folder is
    /// the selected project's, or the shown chat's when the chat is in it,
    /// which may be a worktree; without a project selected, the shown chat's
    /// folder, or the home one.
    pub fn shell(&self) -> Option<(PathBuf, Entity<Terminal>, bool)> {
        let cwd = self.shell_cwd()?;
        let shell = self.shells.get(&cwd)?;
        Some((cwd, shell.terminal.clone(), shell.shown))
    }

    fn active_cwd(&self) -> Option<PathBuf> {
        Some(self.chat(self.active?)?.cwd.to_path_buf())
    }

    /// The folder the shell panel is for (see `shell`).
    fn shell_cwd(&self) -> Option<PathBuf> {
        let selected = self
            .selected
            .and_then(|id| self.projects().iter().find(|project| project.id == id));
        let shows_chat_of = |project: &Project| {
            project
                .chats
                .iter()
                .any(|chat| Some(chat.id) == self.active)
        };
        match selected {
            Some(project) if !shows_chat_of(project) => Some(project.path.clone()),
            _ => self.active_cwd(),
        }
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }

    fn shown_shell_cwd(&self) -> Option<PathBuf> {
        self.shell()
            .and_then(|(cwd, _, shown)| shown.then_some(cwd))
    }

    /// Follows the project the sidebar selected, whose shell the panel is
    /// for.
    pub(crate) fn select_project(&mut self, project: Option<i64>, cx: &App) {
        self.selected = project;
        // Selecting a project is asking to show something, so a new chat or
        // New Chat dialog asked for before doesn't take the view from it.
        self.lifecycle.navigate();
        self.sync_terminals(cx);
    }

    /// Follows whether the settings cover the terminals.
    pub(crate) fn set_covered(&mut self, covered: bool, cx: &App) {
        self.covered = covered;
        self.sync_terminals(cx);
    }

    /// Follows whether the window is closed to the tray, which covers the
    /// terminals and the shown chat. Opened again, the chat is in sight if
    /// the window has the focus and the settings don't cover it.
    pub(crate) fn set_hidden(&mut self, hidden: bool, cx: &mut App) {
        self.hidden = hidden;
        self.sync_terminals(cx);
        let seen = !hidden && cx.active_window().is_some() && !self.covered;
        self.set_focused(seen, cx);
    }

    /// Lists `projects` as the store has them. Which shell shows depends on
    /// the projects' chats, and a new chat shown before they list it got its
    /// project's shell meanwhile.
    pub(crate) fn listed(&mut self, projects: Vec<Project>, cx: &App) {
        let shell = self.shell_cwd();
        self.catalog.listed(projects);
        if self.shell_cwd() != shell {
            self.sync_terminals(cx);
        }
    }

    /// Only the shown chat's CLI and project shell build frames, and neither
    /// does while the settings or the tray cover them.
    pub(crate) fn sync_terminals(&self, cx: &App) {
        let shown = !self.covered && !self.hidden;
        let chat = self.active.filter(|_| shown);
        for each in self.open_chats() {
            if let Some(terminal) = self.terminal(each.id) {
                terminal.read(cx).set_visible(Some(each.id) == chat);
            }
        }
        let shell = self.shown_shell_cwd().filter(|_| shown);
        for (cwd, each) in &self.shells {
            each.terminal
                .read(cx)
                .set_visible(Some(cwd) == shell.as_ref());
        }
    }

    /// Shows the shell the panel is for, starting it if it isn't running, or
    /// hides it if it shows and nothing covers it. Covered, it shows.
    pub(crate) fn toggle_shell(&mut self, cx: &mut Context<Self>) {
        // Removing the project stopped its shell, and nothing would stop
        // another one started before it is gone.
        if self
            .selected
            .is_some_and(|id| self.lifecycle.is_removing_project(id))
        {
            return;
        }
        let Some(cwd) = self.shell_cwd() else {
            return;
        };
        if let Some(shell) = self.shells.get_mut(&cwd)
            && shell.shown
            && !self.covered
            && !self.hidden
        {
            shell.shown = false;
        } else if let Some(shell) = self.shells.get_mut(&cwd) {
            // A shell of this project may be running hidden already.
            shell.shown = true;
        } else {
            let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let colors = self.colors;
            // Like a CLI, the shell gets a scope of its own, so what it
            // leaves running, `nohup` jobs included, stops once it exits.
            let pty = match Pty::spawn(&program, &[], &[], &cwd, colors) {
                Ok(pty) => pty,
                Err(err) => {
                    cx.emit(RuntimeEvent::Notice(format!(
                        "Couldn't start {program}: {err}"
                    )));
                    return;
                }
            };
            let terminal = cx.new(|cx| Terminal::new(pty, cx));
            let exited_cwd = cwd.clone();
            // Shells can exit while the window is closed to the tray.
            let exited = cx.subscribe(&terminal, move |this, _, _: &Exited, cx| {
                this.shell_exited(&exited_cwd, cx)
            });
            self.shells.insert(
                cwd.clone(),
                Shell {
                    terminal,
                    shown: true,
                    _exited: exited,
                },
            );
            cx.emit(RuntimeEvent::ShellStarted(cwd));
        }
        self.sync_terminals(cx);
    }

    /// Closes a shell that exited, and the panel with it.
    fn shell_exited(&mut self, cwd: &Path, cx: &mut Context<Self>) {
        if self.shells.remove(cwd).is_some() {
            cx.emit(RuntimeEvent::ShellEnded(cwd.to_path_buf()));
        }
    }

    /// Kills the shells running in the folders `stops` picks, and gives the
    /// channels that close once each has been reaped.
    fn stop_shells(
        &mut self,
        stops: impl Fn(&Path) -> bool,
        cx: &mut Context<Self>,
    ) -> Vec<Reaped> {
        let stopped: Vec<PathBuf> = self
            .shells
            .keys()
            .filter(|cwd| stops(cwd))
            .cloned()
            .collect();
        let mut reaped = Vec::new();
        for cwd in stopped {
            if let Some(shell) = self.shells.remove(&cwd) {
                reaped.push(shell.terminal.read(cx).shutdown());
                cx.emit(RuntimeEvent::ShellEnded(cwd));
            }
        }
        reaped
    }

    /// Stops the shell of the project in `path`, which is being removed.
    pub(crate) fn stop_project_shell(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.stop_shells(|cwd| cwd == path, cx);
        self.sync_terminals(cx);
    }

    /// Stops the shells of the project in `path`, which is being archived:
    /// its own, and the ones in its chats' `worktrees`.
    pub(crate) fn stop_archived_shells(
        &mut self,
        path: &Path,
        worktrees: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        let stopped = self.stop_shells(
            |cwd| cwd == path || worktrees.iter().any(|root| cwd.starts_with(root)),
            cx,
        );
        if !stopped.is_empty() {
            self.sync_terminals(cx);
        }
    }

    /// Stops the shells running in the worktree a deleted chat ran in, and
    /// removes it once they and its CLI, if it had one (`reaped`), have been
    /// reaped.
    pub(crate) fn remove_chat_worktree(
        &mut self,
        worktree: &Worktree,
        project: &Path,
        reaped: Option<Reaped>,
        cx: &mut Context<Self>,
    ) {
        let stopped = self.stop_shells(|cwd| cwd.starts_with(&worktree.root), cx);
        if !stopped.is_empty() {
            self.sync_terminals(cx);
        }
        let waits = reaped.into_iter().chain(stopped).collect();
        self.remove_worktree(worktree, project, waits, cx);
    }

    /// Kills every shell, as the app quits, and gives the channels that close
    /// once each has been reaped.
    pub(crate) fn shut_down_shells(&self, cx: &App) -> Vec<Reaped> {
        self.shells
            .values()
            .map(|shell| shell.terminal.read(cx).shutdown())
            .collect()
    }

    /// Opens the folder chat `chat` works in, or the shown chat's, with
    /// `editor`, or the default one, and says so if it can't.
    pub(crate) fn open_in_editor(
        &self,
        chat: Option<i64>,
        editor: Option<Editor>,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = chat.or(self.active) else {
            return;
        };
        let Some(editor) = editor.or_else(|| default_editor(cx)) else {
            cx.emit(RuntimeEvent::Notice(
                "No code editor Agents Hub knows is installed".into(),
            ));
            return;
        };
        let Some(folder) = self.chat(id).map(|chat| chat.cwd.to_path_buf()) else {
            return;
        };
        let Some(found) = cx
            .try_global::<Installed>()
            .and_then(|installed| installed.0.iter().find(|each| each.found.editor == editor))
            .map(|each| each.found.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move { editors::open(&found, &folder, terminal::adopt) })
                .await;
            if let Err(err) = opened {
                eprintln!("can't open {}: {err}", editor.name());
                let message = format!("Couldn't open {}: {err}", editor.name());
                this.update(cx, |_, cx| cx.emit(RuntimeEvent::Notice(message)))
                    .ok();
            }
        })
        .detach();
    }
}

/// The editor chats open in: the one the settings name if it is installed,
/// or the first installed one.
fn default_editor(cx: &App) -> Option<Editor> {
    let installed = &cx.try_global::<Installed>()?.0;
    let found: Vec<_> = installed.iter().map(|each| each.found.clone()).collect();
    Some(editors::pick(&found, cx.global::<Settings>().editor)?.editor)
}
