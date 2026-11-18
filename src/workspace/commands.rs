//! What the shortcuts, the search modal and the chat's menu run.

use super::*;

impl Workspace {
    /// The shown chat as the catalog lists it.
    pub(super) fn active_entry<'a>(&'a self, _cx: &'a App) -> Option<&'a store::Chat> {
        let id = self.active?;
        self.catalog
            .projects()
            .iter()
            .flat_map(|project| &project.chats)
            .find(|chat| chat.id == id)
    }

    /// Whether a chat, or its project, is archived.
    pub(super) fn is_archived(&self, id: i64, _cx: &App) -> bool {
        self.catalog.projects().iter().any(|project| {
            project.chats.iter().any(|chat| {
                chat.id == id && (chat.archived_at.is_some() || project.archived_at.is_some())
            })
        })
    }

    /// Whether chat `id`'s project is being removed.
    pub(super) fn is_removing(&self, id: i64, _cx: &App) -> bool {
        self.catalog.projects().iter().any(|project| {
            self.lifecycle.is_removing_project(project.id)
                && project.chats.iter().any(|chat| chat.id == id)
        })
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
        match self.sidebar.read(cx).selected().map(|project| project.id) {
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
        if let Some(chat) = self.chats.iter().find(|chat| Some(chat.id) == self.active) {
            chat.history.update(cx, |history, cx| history.toggle(cx));
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
        if let Some(id) = self.active {
            self.confirm_delete_chat(id, window, cx);
        }
    }

    pub(super) fn archive_active(
        &mut self,
        _: &ArchiveChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active {
            self.confirm_archive_chat(id, window, cx);
        }
    }

    pub(super) fn archive_selected(
        &mut self,
        _: &ArchiveProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.sidebar.read(cx).selected().map(|project| project.id) {
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
        // A chat waiting for its last session to be read, or for its stopped
        // CLI to be reaped, would start once that's done, and so would a new
        // chat once its worktree is checked out. The checkouts stay listed, so
        // shutdown still removes what they make if the app quits first.
        self.lifecycle.close_all();
        for checkout in &self.checkouts {
            checkout.cancel();
        }
        let ids = self.chats.iter().map(|chat| chat.id).collect();
        self.close_chats(ids, window, cx);
    }

    /// Closes the chats whose CLI sits at rest with nothing the user would
    /// miss, the shown one included. Opening one again resumes its session.
    pub(super) fn close_idle_chats(
        &mut self,
        _: &CloseIdleChats,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let asked: Vec<_> = self
            .chats
            .iter()
            .map(|chat| {
                let last_prompt = chat.session.map(|session| self.store.last_prompt(session));
                let transcript = chat.transcript.clone();
                (
                    chat.id,
                    chat.report.clone(),
                    chat.session,
                    transcript,
                    last_prompt,
                )
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut found = Vec::new();
            for (id, report, session, transcript, last_prompt) in asked {
                let last_prompt = match last_prompt {
                    Some(last_prompt) => last_prompt.await,
                    None => Ok((false, None)),
                };
                let kept =
                    smol::unblock(move || transcript.is_some_and(|path| path.exists())).await;
                found.push((id, report, session, kept, last_prompt));
            }
            this.update_in(cx, |this, window, cx| {
                // One that restarted, or went into another session, meanwhile
                // isn't what was read.
                let ids = found
                    .into_iter()
                    .filter(|(id, report, session, kept, last_prompt)| {
                        this.chats
                            .iter()
                            .find(|chat| {
                                chat.id == *id && chat.report == *report && chat.session == *session
                            })
                            .is_some_and(|chat| Self::at_rest(chat, *kept, last_prompt, cx))
                    })
                    .map(|(id, ..)| id)
                    .collect();
                this.close_chats(ids, window, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn close_chats(
        &mut self,
        ids: Vec<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        // Settings in front of the chat keep the keyboard whatever closes,
        // as does the sidebar beside it.
        let chat_focused = self.active.is_some()
            && !self.settings_shown
            && self.chat_focus.contains_focused(window, cx);
        let focused_shell = self
            .shown_shell(cx)
            .filter(|(_, shell)| shell.view.focus_handle(cx).contains_focused(window, cx))
            .map(|(cwd, _)| cwd);
        for id in ids {
            self.suspend(id, cx);
        }
        let shell = self.shown_shell(cx);
        match focused_shell {
            // The shell under the shown chat follows the selected project once
            // the chat is gone, so a worktree's shell gives way to the
            // project's, which takes the keyboard if it's running.
            Some(cwd) if shell.as_ref().map(|(shown, _)| shown) != Some(&cwd) => match shell {
                Some((_, shell)) => shell.view.focus_handle(cx).focus(window, cx),
                None => self.focus_chat(window, cx),
            },
            // The shell still shows and keeps the keyboard.
            Some(_) => {}
            // The shown chat went, its terminal and header with it, and the
            // keyboard with them.
            None if chat_focused && self.active.is_none() => self.focus_chat(window, cx),
            None => {}
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
        if let Some(id) = self.sidebar.read(cx).chat_at(action.0) {
            self.open_chat(id, window, cx);
        }
    }

    pub(super) fn select_project_at(
        &mut self,
        action: &SelectProjectAt,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_at(action.0, cx));
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
        let sidebar = self.sidebar.read(cx);
        let chats: Vec<_> = sidebar
            .running_chats()
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
        let active = self.active;
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
                            sidebar::group_heading(group, count, tone, cx).into_any_element()
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
                            .child(sidebar::dot(tone, cx))
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
        match self.sidebar.read(cx).next_needing_you(self.active) {
            Some(id) => self.open_chat(id, window, cx),
            None => window.push_notification("No agent needs you", cx),
        }
    }
}
