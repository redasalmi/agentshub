//! Starting a new chat, with or without a worktree of its own.

use std::path::PathBuf;

use agentshub_core::agent::Agent;
use gpui_kit::{AppContext as _, Context};

use crate::{Runtime, RuntimeEvent};

impl Runtime {
    /// The agents a project can start, as last looked up for it (see
    /// `Intent::LookUpInstalled`), with its id.
    pub fn installed(&self) -> Option<(i64, &[Agent])> {
        self.installed
            .as_ref()
            .map(|(id, agents)| (*id, agents.as_slice()))
    }

    /// The folder of project `id`.
    fn project_path(&self, id: i64) -> Option<PathBuf> {
        self.projects()
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.path.clone())
    }

    /// Finds the agents project `id` can start, off the foreground thread,
    /// for the buttons that start them while no chat is open.
    pub(crate) fn look_up_installed(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(path) = self.project_path(id) else {
            return;
        };
        self.installed_lookup = Some(cx.spawn(async move |this, cx| {
            let agents = cx
                .background_spawn(async move { Agent::installed(&path) })
                .await;
            this.update(cx, |this, cx| {
                this.installed = Some((id, agents));
                cx.emit(RuntimeEvent::InstalledChanged(id));
            })
            .ok();
        }));
    }
}
