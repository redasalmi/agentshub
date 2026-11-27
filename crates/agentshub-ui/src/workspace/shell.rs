//! The project shell's view and the terminal wiring around the chats.

use super::*;

impl Workspace {
    /// The shell the panel shows, with its folder.
    pub(super) fn shown_shell(&self, cx: &App) -> Option<(PathBuf, Entity<TerminalView>)> {
        let (cwd, _, shown) = self.runtime.read(cx).shell()?;
        let view = self.shell_views.get(&cwd)?.clone();
        shown.then_some((cwd, view))
    }

    /// The shell is the selected project's, so it changes with the selection,
    /// and takes the focus from the one before.
    pub(super) fn project_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.sidebar.read(cx).selected().map(|project| project.id);
        self.apply(Intent::ProjectSelected(selected), cx);
        let shown = self.shown_shell(cx).map(|(_, view)| view.focus_handle(cx));
        let in_shell = self
            .shell_views
            .values()
            .any(|view| view.focus_handle(cx).is_focused(window));
        if in_shell && !shown.as_ref().is_some_and(|focus| focus.is_focused(window)) {
            match shown {
                Some(focus) if !self.settings_shown => focus.focus(window, cx),
                _ => self.focus_chat(window, cx),
            }
        }
        // Selecting a project is asking to show something, so a new chat or
        // New Chat dialog asked for before doesn't take the view from it.
        self.runtime
            .update(cx, |runtime, _| runtime.lifecycle_mut().navigate());
        self.look_up_installed(cx);
        cx.notify();
    }

    /// Has the runtime find the agents the selected project can start, for
    /// the buttons that start them while no chat is open.
    pub(super) fn look_up_installed(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.sidebar.read(cx).selected().map(|project| project.id) {
            self.apply(Intent::LookUpInstalled(id), cx);
        }
    }

    /// Tells the runtime whether the settings cover the terminals, which
    /// then build no frames.
    pub(super) fn cover_terminals(&self, cx: &mut App) {
        self.apply(Intent::SetCovered(self.settings_shown), cx);
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
        // The runtime starts no shell in a project being removed, and nothing
        // changes.
        let selected = self.sidebar.read(cx).selected().map(|project| project.id);
        if selected.is_some_and(|id| self.runtime.read(cx).lifecycle().is_removing_project(id)) {
            return;
        }
        self.apply(Intent::ToggleShell, cx);
        // Without a shell, none could start, so the settings stay up.
        let Some((cwd, terminal, shown)) = self.runtime.read(cx).shell() else {
            return;
        };
        self.settings_shown = false;
        if shown {
            let view = self.shell_view(cwd, terminal, cx);
            view.focus_handle(cx).focus(window, cx);
        } else {
            self.focus_chat(window, cx);
        }
        self.cover_terminals(cx);
        // Closing the settings shows the chat, which no activation reports
        // while the window already has the focus.
        self.set_focused(window.is_window_active(), cx);
        cx.notify();
    }

    /// The view of the shell in `cwd`, built on its terminal the first time.
    fn shell_view(
        &mut self,
        cwd: PathBuf,
        terminal: Entity<Terminal>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        self.shell_views
            .entry(cwd)
            .or_insert_with(|| cx.new(|cx| TerminalView::new(terminal, cx)))
            .clone()
    }

    /// Builds the view of a shell that started, if the panel is for it.
    pub(super) fn shell_started(&mut self, cwd: &Path, cx: &mut Context<Self>) {
        if let Some((shell, terminal, _)) = self.runtime.read(cx).shell()
            && shell == cwd
        {
            self.shell_view(shell, terminal, cx);
        }
        cx.notify();
    }

    /// Drops the view of a shell that ended. The panel goes with it, and the
    /// keyboard moves back to the chat if the shell had it.
    pub(super) fn shell_ended(&mut self, cwd: &Path, cx: &mut Context<Self>) {
        let Some(view) = self.shell_views.remove(cwd) else {
            return;
        };
        // Without a window there's no focus to move.
        cx.with_window(cx.entity_id(), |window, cx| {
            if view.focus_handle(cx).is_focused(window) {
                self.focus_chat(window, cx);
            }
        });
        cx.notify();
    }
}
