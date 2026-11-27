//! Showing the app's catalog in the sidebar.

use super::*;

impl Workspace {
    /// Shows the catalog in the sidebar.
    pub(super) fn sync_catalog(&mut self, cx: &mut Context<Self>) {
        let projects = self.runtime.read(cx).projects().to_vec();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_projects(projects, cx));
    }
}
