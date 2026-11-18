//! The projects and chats as the app knows them, saved or not.

use agentshub_core::store::{Change, Project};
use gpui_kit::Task;

/// The projects and chats, in the order the user put them in, with the changes
/// shown before they are saved.
pub struct Catalog {
    projects: Vec<Project>,
    /// How many changes show that aren't saved yet. The projects are listed
    /// again only once none are, as a listing from before one is saved would
    /// take it back.
    unsaved: usize,
    /// Listing the projects again, which a newer listing replaces.
    listing: Option<Task<()>>,
    /// A project to select once the projects are listed, as one just added
    /// isn't before.
    select_listed: Option<i64>,
}

impl Catalog {
    pub fn new(projects: Vec<Project>) -> Self {
        Self {
            projects,
            unsaved: 0,
            listing: None,
            select_listed: None,
        }
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn unsaved(&self) -> usize {
        self.unsaved
    }

    /// Shows `change` at once, and counts it as unsaved.
    pub fn change(&mut self, change: &Change) {
        change.apply(&mut self.projects);
        self.unsaved += 1;
    }

    /// Counts a change as saved, or as one that couldn't be.
    pub fn saved(&mut self) {
        self.unsaved = self.unsaved.saturating_sub(1);
    }

    /// Keeps the listing under way, replacing an older one.
    pub fn listing(&mut self, listing: Task<()>) {
        self.listing = Some(listing);
    }

    /// Takes the listed projects in place of those shown.
    pub fn listed(&mut self, projects: Vec<Project>) {
        self.projects = projects;
    }

    /// Selects `id` once the projects are listed.
    pub fn select_when_listed(&mut self, id: i64) {
        self.select_listed = Some(id);
    }

    pub fn take_selected(&mut self) -> Option<i64> {
        self.select_listed.take()
    }
}
