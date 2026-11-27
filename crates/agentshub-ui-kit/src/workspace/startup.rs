//! Building a workspace from the database startup opened.

use super::*;

impl Workspace {
    /// The workspace on `runtime`, in the layout `layout` builds for it.
    #[expect(
        clippy::too_many_lines,
        reason = "creates every part of the workspace and the tasks that feed it"
    )]
    pub fn new(
        runtime: Entity<Runtime>,
        layout: impl FnOnce(WeakEntity<Self>, &mut Window, &mut Context<Self>) -> Box<dyn LayoutView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (settings, imported) = runtime.update(cx, |runtime, _| runtime.take_startup());
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
        // Before the terminals take the theme's colors. The observer below
        // can't hear this change: GPUI drops a second notice of the theme
        // while the first is pending, and that one goes out before the
        // observer starts.
        appearance::follow_system(window, cx);
        appearance::apply_motion(cx);
        let layout = layout(cx.weak_entity(), window, cx);

        let runtime_events = cx.subscribe(&runtime, |this, _, event, cx| match event {
            RuntimeEvent::CatalogChanged => {
                this.layout.refresh(cx);
                this.refresh(cx);
            }
            RuntimeEvent::HistoryChanged { sessions } => this.refresh_after(sessions, cx),
            RuntimeEvent::Listed => {
                this.layout.refresh(cx);
                // Which shell shows depends on the projects' chats.
                cx.notify();
            }
            RuntimeEvent::Notice(message) => this.say(message.clone(), cx),
            RuntimeEvent::ChatStarted(id) => this.chat_started(*id, cx),
            RuntimeEvent::ChatExited(id) => this.chat_exited(*id, cx),
            RuntimeEvent::ChatEnded(id) => this.chat_ended(*id, cx),
            RuntimeEvent::StatusChanged(_) => this.layout.refresh(cx),
            RuntimeEvent::ActiveChanged(active) => this.active_changed(*active, cx),
            RuntimeEvent::TurnTick => cx.notify(),
            RuntimeEvent::ClosingAfterLookup(ids) => this.close_after_lookup(ids.clone(), cx),
            RuntimeEvent::InstalledChanged(id) => this.installed_changed(*id, cx),
            RuntimeEvent::CopyingProject { id, project } => {
                this.copying_project(id.clone(), project, cx)
            }
            RuntimeEvent::CopiedProject(id) => this.copied_project(id.clone(), cx),
            RuntimeEvent::SelectProject(id) => this.layout.select_project(*id, cx),
            RuntimeEvent::Undoable { message, undo } => {
                this.offer_undo(message.clone(), undo.clone(), cx)
            }
            RuntimeEvent::ShellStarted(cwd) => this.shell_started(cwd, cx),
            RuntimeEvent::ShellEnded(cwd) => this.shell_ended(cwd, cx),
            RuntimeEvent::ImportChecked { path, checked } => this.import_checked(path, checked, cx),
            RuntimeEvent::Restart => cx.restart(),
        });

        let window_closed = {
            let runtime = runtime.downgrade();
            cx.on_window_closed(move |cx, _| {
                runtime
                    .update(cx, |runtime, cx| runtime.apply(Intent::WindowClosed, cx))
                    .ok();
            })
        };

        let theme = cx.observe_global::<Theme>(|this, cx| {
            let colors = appearance::terminal_colors(cx);
            this.runtime
                .update(cx, |runtime, cx| runtime.set_colors(colors, cx));
        });

        // Nothing has the keyboard once what had it is gone, such as the
        // terminal of a chat closed from a dialog, which hands the focus back
        // to that terminal as it goes. The shortcuts would stop working, so
        // the shown chat, or the workspace, takes it.
        let focus_lost = cx.on_focus_lost(window, |this, window, cx| this.focus_chat(window, cx));

        let settings_view = {
            let this = cx.entity().downgrade();
            cx.new(|_| {
                SettingsView::new(Rc::new(move |intent, window, cx| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.settings_intent(intent, window, cx));
                    }
                }))
            })
        };

        let this = Self {
            layout,
            settings_view,
            runtime,
            _runtime_events: runtime_events,
            _window_closed: window_closed,
            panes: HashMap::new(),
            reveal_numbers: None,
            _new_chat_lookup: None,
            palette_start: None,
            _theme: theme,
            _focus_lost: focus_lost,
            shell_views: HashMap::new(),
            settings_shown: false,
            running_menu: None,
            focus_handle: cx.focus_handle(),
            chat_focus: cx.focus_handle(),
            importing: false,
        };
        let colors = appearance::terminal_colors(cx);
        this.runtime
            .update(cx, |runtime, cx| runtime.set_colors(colors, cx));
        this.set_focused(window.is_window_active(), cx);
        this.layout.refresh(cx);
        this
    }
}
