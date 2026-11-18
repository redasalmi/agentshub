//! The search modal's chats, projects and start entries, from the catalog.

use std::collections::HashMap;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::IconName;
use gpui_kit::*;

use crate::palette::{Entry, Lead, Pick};
use crate::sidebar::{self, Tone, home_relative};

use super::{Workspace, start_entries};

/// The lists the search modal shows before the commands.
pub(super) struct PaletteLists {
    pub chats: Vec<Entry>,
    pub projects: Vec<Entry>,
    pub archived: Vec<Entry>,
    /// Where each chat is, for what the message search finds in it.
    pub places: HashMap<i64, SharedString>,
    pub start_heading: String,
    pub start: Vec<Entry>,
    /// The project whose chats the list shows.
    pub selected: Option<i64>,
}

impl Workspace {
    pub(super) fn palette_lists(&self, cx: &App) -> PaletteLists {
        let sidebar = self.sidebar.read(cx);
        let installed = self.installed.clone();
        let mut chats = Vec::new();
        let mut archived = Vec::new();
        let mut places = HashMap::new();
        for project in self.catalog.projects() {
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

        let selected = sidebar.selected().map(|project| project.id);
        let start_heading = match sidebar.selected() {
            Some(project) => format!("Start an agent in {}", project.name),
            None => "Start an agent".into(),
        };
        let start = selected
            .zip(installed)
            .filter(|(project, (id, _))| project == id)
            .map(|(project, (_, agents))| start_entries(project, &agents))
            .unwrap_or_default();

        for project in self.catalog.projects() {
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

        let projects = self
            .catalog
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

        PaletteLists {
            chats,
            projects,
            archived,
            places,
            start_heading,
            start,
            selected,
        }
    }
}
