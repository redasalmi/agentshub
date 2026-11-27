//! What the shortcuts, the search modal and the chat's menu run.

use super::*;

impl Workspace {
    /// Does what the layout asks for.
    pub fn navigate(&mut self, event: &NavEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            NavEvent::AddProject => self.add_project(window, cx),
            NavEvent::DropPaths(paths) => self.add_dropped(paths, window, cx),
            NavEvent::RemoveProject(id) => self.confirm_remove_project(*id, window, cx),
            NavEvent::ArchiveProject(id) => self.confirm_archive_project(*id, window, cx),
            NavEvent::NewChat(project_id) => self.open_new_chat_dialog(*project_id, window, cx),
            NavEvent::OpenChat(id) => self.open_chat(*id, window, cx),
            NavEvent::RenameChat(id, title) => {
                self.open_rename_dialog(*id, title.clone(), window, cx)
            }
            NavEvent::PinChat(id, pinned) => self.change(
                Change::Pin {
                    chat: *id,
                    pinned: *pinned,
                },
                cx,
            ),
            NavEvent::MoveChat(id, to) => self.change(Change::MoveChat { chat: *id, to: *to }, cx),
            NavEvent::MoveProject(id, to) => self.change(
                Change::MoveProject {
                    project: *id,
                    to: *to,
                },
                cx,
            ),
            NavEvent::CloseChat(id) => self.confirm_close_chat(*id, window, cx),
            NavEvent::DeleteChat(id) => self.confirm_delete_chat(*id, window, cx),
            NavEvent::ArchiveChat(id) => self.confirm_archive_chat(*id, window, cx),
            NavEvent::RestoreChat(id) => self.change(Change::RestoreChat(*id), cx),
            NavEvent::OpenSettings => self.show_settings(true, window, cx),
            NavEvent::Selected => self.project_selected(window, cx),
            NavEvent::OpenSearch => self.open_palette(false, window, cx),
            NavEvent::RunningMenu(open) => {
                if *open {
                    self.open_running_menu(window, cx);
                    if let Some(menu) = &self.running_menu {
                        menu.focus_handle(cx).focus(window, cx);
                    }
                } else {
                    self.running_menu = None;
                    cx.notify();
                }
            }
            NavEvent::CloseWindow => {
                agentshub_platform::tray::remember_bounds(window.window_bounds(), cx);
                window.remove_window();
            }
        }
    }

    /// The shown chat as the catalog lists it.
    pub(super) fn active_entry<'a>(&'a self, cx: &'a App) -> Option<&'a store::Chat> {
        let id = self.runtime.read(cx).active()?;
        self.runtime
            .read(cx)
            .projects()
            .iter()
            .flat_map(|project| &project.chats)
            .find(|chat| chat.id == id)
    }

    pub(super) fn open_search(
        &mut self,
        _: &OpenSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(false, window, cx);
    }

    pub(super) fn show_archived(
        &mut self,
        _: &ShowArchived,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(true, window, cx);
    }

    /// Starts an agent in the selected project, or asks for a project when
    /// there's none.
    pub(super) fn new_chat_here(
        &mut self,
        _: &NewChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.layout.selected_project(cx) {
            Some(id) => self.open_new_chat_dialog(id, window, cx),
            None => self.add_project(window, cx),
        }
    }

    pub(super) fn add_project_action(
        &mut self,
        _: &AddProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_project(window, cx);
    }

    pub(super) fn toggle_settings(
        &mut self,
        _: &ToggleSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_settings(!self.settings_shown, window, cx);
    }

    pub(super) fn toggle_history(
        &mut self,
        _: &ToggleHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let history = self
            .runtime
            .read(cx)
            .active()
            .and_then(|id| self.panes.get(&id))
            .map(|pane| pane.history.clone());
        if let Some(history) = history {
            history.update(cx, |history, cx| history.toggle(cx));
            cx.notify();
        }
    }

    pub(super) fn rename_active(
        &mut self,
        _: &RenameChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(chat) = self.active_entry(cx) {
            let (id, title) = (chat.id, chat.title.clone().unwrap_or_default());
            self.open_rename_dialog(id, title.into(), window, cx);
        }
    }

    pub(super) fn toggle_pin(&mut self, _: &TogglePin, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.active_entry(cx) {
            let (id, pinned) = (chat.id, chat.pinned);
            self.change(
                Change::Pin {
                    chat: id,
                    pinned: !pinned,
                },
                cx,
            );
        }
    }

    pub(super) fn delete_active(
        &mut self,
        _: &DeleteChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.runtime.read(cx).active() {
            self.confirm_delete_chat(id, window, cx);
        }
    }

    pub(super) fn archive_active(
        &mut self,
        _: &ArchiveChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.runtime.read(cx).active() {
            self.confirm_archive_chat(id, window, cx);
        }
    }

    pub(super) fn archive_selected(
        &mut self,
        _: &ArchiveProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.layout.selected_project(cx) {
            self.confirm_archive_project(id, window, cx);
        }
    }

    /// Closes every open chat, stopping its CLI even mid-turn, and the ones
    /// still opening or waiting for their worktree. Opening a chat again
    /// resumes its session.
    pub(super) fn close_all_chats(
        &mut self,
        _: &CloseAllChats,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_closing(Intent::CloseAll, window, cx);
    }

    /// Closes the chats whose CLI sits at rest with nothing the user would
    /// miss, the shown one included. Opening one again resumes its session.
    pub(super) fn close_idle_chats(
        &mut self,
        _: &CloseIdleChats,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let check = self.runtime.read(cx).check_idle();
        cx.spawn_in(window, async move |this, cx| {
            let check = check.await;
            this.update_in(cx, |this, window, cx| {
                this.apply_closing(Intent::CloseIdle(check), window, cx)
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn close_chats(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut App) {
        if ids.is_empty() {
            return;
        }
        self.apply_closing(Intent::CloseChats(ids), window, cx);
    }

    /// Asks the runtime for `intent`, which closes chats, and moves the
    /// keyboard on from what had it if that went with them.
    pub(super) fn apply_closing(&mut self, intent: Intent, window: &mut Window, cx: &mut App) {
        // Settings in front of the chat keep the keyboard whatever closes,
        // as does the sidebar beside it.
        let chat_focused = self.runtime.read(cx).active().is_some()
            && !self.settings_shown
            && self.chat_focus.contains_focused(window, cx);
        let focused_shell = self
            .shown_shell(cx)
            .filter(|(_, view)| view.focus_handle(cx).contains_focused(window, cx))
            .map(|(cwd, _)| cwd);
        self.apply(intent, cx);
        let shell = self.shown_shell(cx);
        match focused_shell {
            // The shell under the shown chat follows the selected project once
            // the chat is gone, so a worktree's shell gives way to the
            // project's, which takes the keyboard if it's running.
            Some(cwd) if shell.as_ref().map(|(shown, _)| shown) != Some(&cwd) => match shell {
                Some((_, view)) => view.focus_handle(cx).focus(window, cx),
                None => self.focus_chat(window, cx),
            },
            // The shown chat went, its terminal and header with it, and the
            // keyboard with them.
            None if chat_focused && self.runtime.read(cx).active().is_none() => {
                self.focus_chat(window, cx)
            }
            // The shell still shows and keeps the keyboard, or the keyboard
            // was elsewhere.
            Some(_) | None => {}
        }
    }

    pub(super) fn toggle_theme(
        &mut self,
        _: &ToggleTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_appearance(appearance::other(cx), window, cx);
    }

    pub(super) fn use_theme(
        &mut self,
        action: &UseTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_theme(action.0, window, cx);
    }

    pub(super) fn export_action(
        &mut self,
        _: &ExportDatabase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_database(window, cx);
    }

    pub(super) fn import_action(
        &mut self,
        _: &ImportDatabase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.import_database(window, cx);
    }

    pub(super) fn open_chat_at(
        &mut self,
        action: &OpenChatAt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.layout.chat_at(action.0, cx) {
            self.open_chat(id, window, cx);
        }
    }

    pub(super) fn select_project_at(
        &mut self,
        action: &SelectProjectAt,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.select_project_at(action.0, cx);
    }

    pub(super) fn show_running(
        &mut self,
        _: &ShowRunning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running_menu.take().is_some() {
            cx.notify();
        } else {
            self.open_running_menu(window, cx);
        }
    }

    /// Opens the menu of the running chats of every project, under what
    /// their agents need, the most urgent first.
    pub(super) fn open_running_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let runtime = self.runtime.read(cx);
        let running = agentshub_ui_kit::layout::running(runtime);
        let chats: Vec<_> = agentshub_ui_kit::layout::running_chats(runtime.projects(), &running)
            .into_iter()
            .map(|(project, chat, tone)| {
                let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                let about = sidebar::subtitle(chat, tone, Some(&project.name));
                (
                    chat.id,
                    SharedString::from(title),
                    SharedString::from(about),
                    tone,
                )
            })
            .collect();
        let active = self.runtime.read(cx).active();
        let workspace = cx.weak_entity();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            menu = menu.min_w(px(300.)).max_w(px(400.)).scrollable(true);
            if chats.is_empty() {
                return menu.item(
                    PopupMenuItem::element(|_, cx| {
                        div()
                            .py_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No agents running")
                    })
                    .disabled(true),
                );
            }
            let mut heading = None;
            for (id, title, about, tone) in chats.iter().cloned() {
                let group = tone.group();
                if heading != Some(group) {
                    heading = Some(group);
                    let count = chats.iter().filter(|chat| chat.3.group() == group).count();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            group_heading(group, count, tone, cx).into_any_element()
                        })
                        .disabled(true),
                    );
                }
                let workspace = workspace.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, cx| {
                        let theme = cx.theme();
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .py_1()
                            .gap(px(10.))
                            .child(dot(tone, cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .truncate()
                                            .when(active == Some(id), |this| this.font_medium())
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(tone.text_color(cx))
                                            .truncate()
                                            .child(about.clone()),
                                    ),
                            )
                    })
                    .on_click(move |_, window, cx| {
                        workspace
                            .update(cx, |this, cx| this.open_chat(id, window, cx))
                            .ok();
                    }),
                );
            }
            menu
        });
        cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.running_menu = None;
            cx.notify();
        })
        .detach();
        self.running_menu = Some(menu);
        cx.notify();
    }

    /// Opens the next chat whose agent needs the user, going round them all
    /// when pressed again.
    pub(super) fn next_needing_you(
        &mut self,
        _: &NextNeedingYou,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.runtime.read(cx).active();
        match self.layout.next_needing_you(active, cx) {
            Some(id) => self.open_chat(id, window, cx),
            None => window.push_notification("No agent needs you", cx),
        }
    }
}
