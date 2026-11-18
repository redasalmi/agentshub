//! Building a workspace from the database startup opened.

use super::*;

impl Workspace {
    pub fn new(opened: Opened, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Opened {
            store,
            lock,
            imported,
            settings,
            projects,
            broken,
        } = opened;
        // Said once the window is up, since its notifications aren't yet.
        if let Some(imported) = imported {
            let message = match imported {
                store::Imported::Replaced(Some(backup)) => format!(
                    "Imported the database. The one it replaced is backed up at {}",
                    home_relative(&backup)
                ),
                store::Imported::Replaced(None) => "Imported the database.".into(),
                store::Imported::Failed(err) => {
                    eprintln!("can't import the database: {err:#}");
                    format!("Couldn't import the database, so it is as it was: {err:#}")
                }
            };
            cx.spawn_in(window, async move |_, cx| {
                cx.update(|window, cx| window.push_notification(message, cx))
                    .ok();
            })
            .detach();
        }

        cx.set_global(settings);
        appearance::apply_motion(cx);
        let sidebar = cx.new(Sidebar::new);
        let sidebar_events =
            cx.subscribe_in(&sidebar, window, |this, _, event, window, cx| match event {
                SidebarEvent::AddProject => this.add_project(window, cx),
                SidebarEvent::DropPaths(paths) => this.add_dropped(paths, window, cx),
                SidebarEvent::RemoveProject(id) => this.confirm_remove_project(*id, window, cx),
                SidebarEvent::ArchiveProject(id) => this.confirm_archive_project(*id, window, cx),
                SidebarEvent::NewChat(project_id) => {
                    this.open_new_chat_dialog(*project_id, window, cx)
                }
                SidebarEvent::OpenChat(id) => this.open_chat(*id, window, cx),
                SidebarEvent::RenameChat(id, title) => {
                    this.open_rename_dialog(*id, title.clone(), window, cx)
                }
                SidebarEvent::PinChat(id, pinned) => this.change(
                    Change::Pin {
                        chat: *id,
                        pinned: *pinned,
                    },
                    cx,
                ),
                SidebarEvent::MoveChat(id, to) => {
                    this.change(Change::MoveChat { chat: *id, to: *to }, cx)
                }
                SidebarEvent::MoveProject(id, to) => this.change(
                    Change::MoveProject {
                        project: *id,
                        to: *to,
                    },
                    cx,
                ),
                SidebarEvent::CloseChat(id) => this.confirm_close_chat(*id, window, cx),
                SidebarEvent::DeleteChat(id) => this.confirm_delete_chat(*id, window, cx),
                SidebarEvent::ArchiveChat(id) => this.confirm_archive_chat(*id, window, cx),
                SidebarEvent::RestoreChat(id) => this.change(Change::RestoreChat(*id), cx),
                SidebarEvent::OpenSettings => this.show_settings(true, window, cx),
                SidebarEvent::ShowProjectNames(show) => this.show_project_names(*show, window, cx),
                SidebarEvent::Selected => this.project_selected(window, cx),
            });

        let (reports, reported) = Reports::new();
        let report_events = cx.spawn(async move |this, cx| {
            while let Ok(key) = reported.recv().await {
                if this
                    .update(cx, |this, cx| this.read_reports(&key, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let (codex_watcher, codex_events) = codex::watch_sessions(cx);

        // The workspace outlives its window (see `tray`), so it's released
        // when the app quits.
        cx.on_release(|this, cx| this.shutdown(cx)).detach();

        let theme = cx.observe_global::<Theme>(|this, cx| {
            let colors = appearance::terminal_colors(cx);
            for chat in &this.chats {
                if let Some(terminal) = &chat.pane.terminal {
                    terminal.terminal.read(cx).set_colors(colors);
                }
            }
            for shell in this.shells.values() {
                shell.terminal.read(cx).set_colors(colors);
            }
        });

        // Nothing has the keyboard once what had it is gone, such as the
        // terminal of a chat closed from a dialog, which hands the focus back
        // to that terminal as it goes. The shortcuts would stop working, so
        // the shown chat, or the workspace, takes it.
        let focus_lost = cx.on_focus_lost(window, |this, window, cx| this.focus_chat(window, cx));

        let mut this = Self {
            sidebar,
            store,
            catalog: Catalog::new(projects.clone()),
            chats: Vec::new(),
            active: None,
            lifecycle: Lifecycle::default(),
            reports,
            codex_watcher,
            codex_watch: None,
            _sidebar_events: sidebar_events,
            _report_events: report_events,
            _codex_events: codex_events,
            turn_tick: None,
            reveal_numbers: None,
            installed: None,
            _installed_lookup: None,
            _new_chat_lookup: None,
            palette_start: None,
            _theme: theme,
            _focus_lost: focus_lost,
            broken: broken.map(SharedString::from),
            reading: Vec::new(),
            reading_last: Vec::new(),
            checkouts: Vec::new(),
            _lock: lock,
            shells: HashMap::new(),
            settings_shown: false,
            running_menu: None,
            hidden: false,
            window: window.window_handle(),
            focus_handle: cx.focus_handle(),
            chat_focus: cx.focus_handle(),
            sidebar_width: px(cx.global::<Settings>().sidebar_width),
            importing: false,
        };
        this.update_codex_watch();
        this.sidebar
            .update(cx, |sidebar, cx| sidebar.set_projects(projects, cx));
        this.catch_up(cx);
        this
    }

    /// Reads what the CLIs wrote since the transcripts were last read, so
    /// that the search finds it without each chat having to be opened first.
    fn catch_up(&self, cx: &mut Context<Self>) {
        let behind = self.store.transcripts_behind();
        cx.spawn(async move |this, cx| {
            match behind.await {
                Ok(sessions) => this
                    .update(cx, |this, cx| {
                        this.read_sessions(sessions, false, cx).detach()
                    })
                    .ok(),
                Err(err) => {
                    eprintln!("can't list the transcripts to read: {err}");
                    None
                }
            };
        })
        .detach();
    }
}
