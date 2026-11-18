//! The project shell and the terminal wiring around the chats.

use super::*;

impl Workspace {
    pub(super) fn active_cwd(&self) -> Option<&Path> {
        self.chats
            .iter()
            .find(|chat| Some(chat.id) == self.active)
            .map(|chat| chat.cwd.as_path())
    }

    /// The folder of the shell that shows: the selected project, or the
    /// shown chat's folder when the chat is in it, which may be a worktree;
    /// without a project selected, the shown chat's folder, or the home one.
    pub(super) fn shell_cwd(&self, cx: &App) -> Option<PathBuf> {
        let selected = self.sidebar.read(cx).selected();
        let shows_chat_of = |project: &store::Project| {
            project
                .chats
                .iter()
                .any(|chat| Some(chat.id) == self.active)
        };
        match selected {
            Some(project) if !shows_chat_of(project) => Some(project.path.clone()),
            _ => self.active_cwd().map(Path::to_path_buf),
        }
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }

    pub(super) fn shown_shell(&self, cx: &App) -> Option<(PathBuf, &Shell)> {
        let cwd = self.shell_cwd(cx)?;
        let shell = self.shells.get(&cwd)?;
        shell.shown.then_some((cwd, shell))
    }

    /// The shell is the selected project's, so it changes with the selection,
    /// and takes the focus from the one before.
    pub(super) fn project_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self
            .shown_shell(cx)
            .map(|(_, shell)| shell.view.focus_handle(cx));
        let in_shell = self
            .shells
            .values()
            .any(|shell| shell.view.focus_handle(cx).is_focused(window));
        if in_shell && !shown.as_ref().is_some_and(|focus| focus.is_focused(window)) {
            match shown {
                Some(focus) if !self.settings_shown => focus.focus(window, cx),
                _ => self.focus_chat(window, cx),
            }
        }
        // Selecting a project is asking to show something, so a new chat or
        // New Chat dialog asked for before doesn't take the view from it.
        self.lifecycle.navigate();
        self.sync_terminals(cx);
        self.look_up_installed(cx);
        cx.notify();
    }

    /// Finds the agents the selected project can start, for the buttons that
    /// start them while no chat is open.
    pub(super) fn look_up_installed(&mut self, cx: &mut Context<Self>) {
        let Some((id, path)) = self
            .sidebar
            .read(cx)
            .selected()
            .map(|project| (project.id, project.path.clone()))
        else {
            self.installed = None;
            return;
        };
        self._installed_lookup = Some(cx.spawn(async move |this, cx| {
            let agents = cx
                .background_spawn(async move { Agent::installed(&path) })
                .await;
            this.update(cx, |this, cx| {
                if let Some((project, sections)) = &this.palette_start
                    && *project == id
                {
                    sections.set(PALETTE_START, start_entries(id, &agents), cx);
                }
                this.installed = Some((id, agents));
                cx.notify();
            })
            .ok();
        }));
    }

    /// Only the shown chat's CLI and project shell build frames, and neither
    /// does while the settings cover them or the window is closed to the tray.
    pub(super) fn sync_terminals(&self, cx: &App) {
        let shown = !self.hidden && !self.settings_shown;
        let chat = self.active.filter(|_| shown);
        for each in &self.chats {
            if let Some(terminal) = &each.pane.terminal {
                terminal
                    .terminal
                    .read(cx)
                    .set_visible(Some(each.id) == chat);
            }
        }
        let shell = self.shown_shell(cx).map(|(cwd, _)| cwd).filter(|_| shown);
        for (cwd, each) in &self.shells {
            each.terminal
                .read(cx)
                .set_visible(Some(cwd) == shell.as_ref());
        }
    }

    /// Has the workspace shown in `window`, opened again from the tray.
    pub fn set_window(&mut self, window: &Window) {
        self.window = window.window_handle();
    }

    /// Stops the shown terminals from building frames while the window is
    /// closed to the tray, and starts them again when it opens.
    pub fn set_hidden(&mut self, hidden: bool, cx: &mut App) {
        self.hidden = hidden;
        self.sync_terminals(cx);
        self.withdraw_seen(cx.active_window().is_some(), cx);
    }

    /// Shows the shell of the selected project, starting it if it isn't
    /// running, or hides it if it shows. Settings cover the shell, so they
    /// close and the shell shows.
    pub(super) fn toggle_shell(
        &mut self,
        _: &ToggleShell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Removing the project stopped its shell, and nothing would stop
        // another one started before it is gone.
        let selected = self.sidebar.read(cx).selected().map(|project| project.id);
        if selected.is_some_and(|id| self.lifecycle.is_removing_project(id)) {
            return;
        }
        let Some(cwd) = self.shell_cwd(cx) else {
            return;
        };
        let settings_were_shown = std::mem::take(&mut self.settings_shown);
        if !settings_were_shown && self.shells.get(&cwd).is_some_and(|shell| shell.shown) {
            if let Some(shell) = self.shells.get_mut(&cwd) {
                shell.shown = false;
            }
            self.focus_chat(window, cx);
        } else {
            if !self.shells.contains_key(&cwd) {
                let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let colors = appearance::terminal_colors(cx);
                // Like a CLI, the shell gets a scope of its own, so what it
                // leaves running, `nohup` jobs included, stops once it exits.
                let pty = match Pty::spawn(&program, &[], &[], &cwd, colors) {
                    Ok(pty) => pty,
                    Err(err) => {
                        // Nothing changes, so the settings stay up.
                        self.settings_shown = settings_were_shown;
                        window.push_notification(format!("Couldn't start {program}: {err}"), cx);
                        return;
                    }
                };
                let terminal = cx.new(|cx| Terminal::new(pty, cx));
                let view = cx.new(|cx| TerminalView::new(terminal.clone(), cx));
                let exited_cwd = cwd.clone();
                // Shells can exit while the window is closed to the tray.
                let exited = cx.subscribe(&terminal, move |this, _, _: &Exited, cx| {
                    this.shell_exited(&exited_cwd, cx)
                });
                self.shells.insert(
                    cwd.clone(),
                    Shell {
                        terminal,
                        view,
                        shown: true,
                        _exited: exited,
                    },
                );
            }
            let shell = self
                .shells
                .get_mut(&cwd)
                .expect("the shell was just started");
            // A shell of this project may be running hidden already.
            shell.shown = true;
            shell.view.focus_handle(cx).focus(window, cx);
        }
        self.sync_terminals(cx);
        // Closing the settings shows the chat, which no activation reports
        // while the window already has the focus.
        self.withdraw_seen(window.is_window_active(), cx);
        cx.notify();
    }

    /// Closes a shell that exited. The panel goes with it, and the keyboard
    /// moves back to the chat if the shell had it.
    fn shell_exited(&mut self, cwd: &Path, cx: &mut Context<Self>) {
        let Some(shell) = self.shells.remove(cwd) else {
            return;
        };
        if shell.shown {
            // Without a window there's no focus to move.
            cx.with_window(cx.entity_id(), |window, cx| {
                if shell.view.focus_handle(cx).is_focused(window) {
                    self.focus_chat(window, cx);
                }
            });
        }
        cx.notify();
    }
}
