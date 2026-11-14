//! The search modal's sections, from what the workspace holds.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::{ActiveTheme as _, IconName};
use gpui_kit::*;

use crate::palette::{self, Entry, Lead, Pick};
use crate::settings::{DEFAULT_TERMINAL_FONT_SIZE, Settings, ThemeFamily};
use crate::sidebar::{self, Filter, Tone, home_relative};
use crate::store::Change;
use crate::terminal_view;

use super::{
    AddProject, ArchiveChat, ArchiveProject, CloseAllChats, CloseIdleChats, DeleteChat,
    ExportDatabase, ImportDatabase, NextNeedingYou, PALETTE_START, RenameChat, ShowArchived,
    ShowRunning, ShowShortcuts, ToggleHistory, TogglePin, ToggleProjectNames, ToggleSettings,
    ToggleShell, ToggleTheme, UseTheme, Workspace, start_entries,
};

impl Workspace {
    /// Opens the search modal over the chats of every project, the agents
    /// the selected project can start, the projects, the commands, and what
    /// is archived; or with `archived_only`, over what is archived alone.
    pub(super) fn open_palette(
        &mut self,
        archived_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The agents to start are the ones last found until they have been
        // looked up again.
        self.palette_start = None;
        let installed = self.installed.clone();
        self.look_up_installed(cx);
        let sidebar = self.sidebar.read(cx);
        let mut chats = Vec::new();
        let mut archived = Vec::new();
        let mut places = HashMap::new();
        for project in sidebar.projects() {
            for chat in &project.chats {
                // An archived project's chats are archived along with it.
                if let Some(at) = chat.archived_at.or(project.archived_at) {
                    let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                    places.insert(
                        chat.id,
                        SharedString::from(format!("archived · {title} · {}", project.name)),
                    );
                    archived.push((
                        at,
                        Entry::new(title, Lead::Tone(Tone::Saved), Pick::OpenChat(chat.id))
                            .meta(format!(
                                "{} · archived {} · {}",
                                chat.agent.label(),
                                sidebar::age(at),
                                project.name
                            ))
                            .keywords([chat.agent.display_name(), "archived"]),
                    ));
                    continue;
                }
                let tone = Tone::of(sidebar.running(chat.id));
                let doing = match tone {
                    Tone::Saved => sidebar::age(chat.updated_at),
                    tone => tone.label().to_string(),
                };
                let title = chat.title.clone().unwrap_or_else(|| "New chat".into());
                places.insert(
                    chat.id,
                    SharedString::from(format!("{title} · {}", project.name)),
                );
                chats.push((
                    tone.needs_you(),
                    Entry::new(title, Lead::Tone(tone), Pick::OpenChat(chat.id))
                        .meta(format!(
                            "{} · {doing} · {}",
                            chat.agent.label(),
                            project.name
                        ))
                        .keywords([chat.agent.display_name()]),
                ));
            }
        }
        // The chats that need the user come first.
        chats.sort_by_key(|(needs_you, _)| !needs_you);
        let chats = chats.into_iter().map(|(_, entry)| entry).collect();

        let selected = sidebar.selected();
        let start_heading = match selected {
            Some(project) => format!("Start an agent in {}", project.name),
            None => "Start an agent".into(),
        };
        let selected_id = selected.map(|project| project.id);
        let start = selected_id
            .zip(installed)
            .filter(|(project, (id, _))| project == id)
            .map(|(project, (_, agents))| start_entries(project, &agents))
            .unwrap_or_default();

        for project in sidebar.projects() {
            if let Some(at) = project.archived_at {
                archived.push((
                    at,
                    Entry::new(
                        project.name.clone(),
                        Lead::Icon(ExtraIcon::Archive),
                        Pick::SelectProject(project.id),
                    )
                    .meta(format!(
                        "archived {} · {}",
                        sidebar::age(at),
                        home_relative(&project.path)
                    ))
                    .keywords(["project", "archived"]),
                ));
            }
        }
        // The last archived first.
        archived.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        let archived: Vec<_> = archived.into_iter().map(|(_, entry)| entry).collect();

        let this = cx.entity().downgrade();
        let on_pick = move |pick, window: &mut Window, cx: &mut App| {
            this.update(cx, |this, cx| this.run_pick(pick, window, cx))
                .ok();
        };
        if archived_only {
            palette::open(
                vec![("Archived".into(), archived)],
                None,
                on_pick,
                window,
                cx,
            );
            return;
        }

        let projects = sidebar
            .projects()
            .iter()
            .filter(|project| project.archived_at.is_none())
            .map(|project| {
                Entry::new(
                    project.name.clone(),
                    Lead::Icon(IconName::FolderOpen.into()),
                    Pick::SelectProject(project.id),
                )
                .meta(home_relative(&project.path))
                .keywords(["project"])
            })
            .collect();

        let run = |label: &str, icon: IconName, action: Box<dyn Action>| {
            Entry::new(
                label.to_string(),
                Lead::Icon(icon.into()),
                Pick::Run(action),
            )
        };
        let mut commands = Vec::new();
        if let Some(chat) = self.active_entry(cx) {
            commands.push(run(
                "Show or hide the earlier sessions",
                IconName::Inbox,
                Box::new(ToggleHistory),
            ));
            commands.push(run(
                "Show or hide the project's shell",
                IconName::SquareTerminal,
                Box::new(ToggleShell),
            ));
            commands.push(run(
                "Rename this chat",
                IconName::Replace,
                Box::new(RenameChat),
            ));
            commands.push(run(
                if chat.pinned {
                    "Unpin this chat"
                } else {
                    "Pin this chat to the top"
                },
                IconName::Star,
                Box::new(TogglePin),
            ));
            commands.push(
                Entry::new(
                    "Archive this chat",
                    Lead::Icon(ExtraIcon::Archive),
                    Pick::Run(Box::new(ArchiveChat)),
                )
                .keywords(["hide"]),
            );
            commands.push(run(
                "Delete this chat",
                IconName::Delete,
                Box::new(DeleteChat),
            ));
        }
        if selected.is_some() {
            commands.push(
                Entry::new(
                    "Archive this project",
                    Lead::Icon(ExtraIcon::Archive),
                    Pick::Run(Box::new(ArchiveProject)),
                )
                .keywords(["hide"]),
            );
        }
        if !archived.is_empty() {
            commands.push(Entry::new(
                "Show the archived chats and projects",
                Lead::Icon(ExtraIcon::ArchiveRestore),
                Pick::Run(Box::new(ShowArchived)),
            ));
        }
        if !self.chats.is_empty() {
            commands.push(
                run(
                    "Close the idle chats",
                    IconName::Close,
                    Box::new(CloseIdleChats),
                )
                .keywords(["stop"]),
            );
            commands.push(
                run(
                    "Close all the chats",
                    IconName::Close,
                    Box::new(CloseAllChats),
                )
                .keywords(["stop"]),
            );
        }
        let filter = sidebar.filter();
        for each in Filter::ALL.into_iter().filter(|each| *each != filter) {
            let label = match each {
                Filter::All => "Show all the chats",
                Filter::NeedsYou => "Show the chats that need you",
                Filter::Running => "Show the running chats",
            };
            commands.push(Entry::new(
                label,
                Lead::Icon(IconName::Search.into()),
                Pick::Filter(each),
            ));
        }
        commands.push(
            Entry::new(
                "Show the running chats of every project",
                Lead::Icon(ExtraIcon::Activity),
                Pick::Run(Box::new(ShowRunning)),
            )
            .keywords(["working", "idle", "active"]),
        );
        commands.push(
            Entry::new(
                "Open the next chat that needs you",
                Lead::Icon(ExtraIcon::CircleAlert),
                Pick::Run(Box::new(NextNeedingYou)),
            )
            .keywords(["waiting", "permission", "question", "done", "error"]),
        );
        commands.push(run(
            "Add a project",
            IconName::FolderOpen,
            Box::new(AddProject),
        ));
        commands.push(run(
            if cx.global::<Settings>().project_names {
                "Hide the projects' names"
            } else {
                "Show the projects' names"
            },
            if cx.global::<Settings>().project_names {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeftOpen
            },
            Box::new(ToggleProjectNames),
        ));
        commands.push(run(
            if cx.theme().is_dark() {
                "Use the light theme"
            } else {
                "Use the dark theme"
            },
            if cx.theme().is_dark() {
                IconName::Sun
            } else {
                IconName::Moon
            },
            Box::new(ToggleTheme),
        ));
        let family = cx.global::<Settings>().theme;
        for each in ThemeFamily::ALL.into_iter().filter(|each| *each != family) {
            commands.push(
                run(
                    &format!("Use the {} theme", each.label()),
                    IconName::Palette,
                    Box::new(UseTheme(each)),
                )
                .keywords(["theme", "colors"]),
            );
        }
        commands.push(
            run(
                "Make the terminals' text bigger",
                IconName::Plus,
                Box::new(terminal_view::ZoomIn),
            )
            .keywords(["zoom", "font", "size", "larger"]),
        );
        commands.push(
            run(
                "Make the terminals' text smaller",
                IconName::Minus,
                Box::new(terminal_view::ZoomOut),
            )
            .keywords(["zoom", "font", "size"]),
        );
        if cx.global::<Settings>().terminal_font_size != DEFAULT_TERMINAL_FONT_SIZE {
            commands.push(
                run(
                    "Reset the terminals' text size",
                    IconName::Replace,
                    Box::new(terminal_view::ResetZoom),
                )
                .keywords(["zoom", "font", "size"]),
            );
        }
        commands.push(run(
            "Settings",
            IconName::Settings,
            Box::new(ToggleSettings),
        ));
        commands.push(
            run(
                "Show the keyboard shortcuts",
                IconName::Info,
                Box::new(ShowShortcuts),
            )
            .keywords(["keys", "keybindings", "help"]),
        );
        commands.push(run(
            "Export a copy of the database",
            IconName::HardDrive,
            Box::new(ExportDatabase),
        ));
        commands.push(
            run(
                "Import a database",
                IconName::HardDrive,
                Box::new(ImportDatabase),
            )
            .keywords(["restore", "backup"]),
        );

        let mut sections = vec![
            ("Chats".into(), chats),
            ("Projects".into(), projects),
            ("Commands".into(), commands),
            ("Archived".into(), archived),
        ];
        sections.insert(PALETTE_START, (start_heading.into(), start));
        let sections = palette::open(
            sections,
            Some(self.message_search(places)),
            on_pick,
            window,
            cx,
        );
        self.palette_start = selected_id.map(|id| (id, sections));
    }

    /// Finds what was said in the chats, off the UI thread. `places` says
    /// where each chat is, to show beside what was found in it.
    fn message_search(&self, places: HashMap<i64, SharedString>) -> palette::Search {
        let store = self.store.clone();
        let places = Rc::new(places);
        Rc::new(move |query, cx| {
            let hits = store.search(query);
            let places = places.clone();
            cx.spawn(async move |_| {
                let hits = hits.await.unwrap_or_else(|err| {
                    eprintln!("can't search the messages: {err}");
                    Vec::new()
                });
                hits.into_iter()
                    .filter_map(|hit| {
                        let place = places.get(&hit.chat_id)?.clone();
                        let lead = if hit.prompt {
                            ExtraIcon::User
                        } else {
                            ExtraIcon::Bot
                        };
                        Some(
                            Entry::new(hit.snippet, Lead::Icon(lead), Pick::OpenChat(hit.chat_id))
                                .meta(place),
                        )
                    })
                    .collect()
            })
        })
    }

    fn run_pick(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            Pick::OpenChat(id) => self.open_chat(id, window, cx),
            Pick::StartAgent(project_id, agent) => self.start_agent(project_id, agent, window, cx),
            Pick::SelectProject(id) => {
                let archived = self
                    .sidebar
                    .read(cx)
                    .projects()
                    .iter()
                    .any(|project| project.id == id && project.archived_at.is_some());
                if archived {
                    self.change(Change::RestoreProject(id), cx);
                }
                self.sidebar
                    .update(cx, |sidebar, cx| sidebar.select(id, cx))
            }
            Pick::Filter(filter) => self
                .sidebar
                .update(cx, |sidebar, cx| sidebar.set_filter(filter, cx)),
            // Sent from where the keys would have sent it: closing the search
            // gave the focus back to where it was, as the shell, unless it was
            // outside the workspace.
            Pick::Run(action) => {
                if !self.focus_handle.contains_focused(window, cx) {
                    self.focus_chat(window, cx);
                }
                window.dispatch_action(action, cx);
            }
        }
    }
}
