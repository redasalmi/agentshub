//! The dialogs for renaming a chat and starting a new one.

use super::*;
use crate::dialogs::{self, NewChatArgs};

impl Workspace {
    pub(super) fn open_rename_dialog(
        &mut self,
        id: i64,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        dialogs::rename(window, cx, title, move |title, cx| {
            this.update(cx, |this, cx| {
                this.change(Change::Rename { chat: id, title }, cx)
            })
            .ok();
        });
    }

    pub(super) fn open_new_chat_dialog(
        &mut self,
        project_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .runtime
            .read(cx)
            .projects()
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            return;
        };
        let (asked, lookup) = self
            .runtime
            .update(cx, |runtime, cx| runtime.ask_for_new_chat(&project, cx));
        self._new_chat_lookup = Some(cx.spawn_in(window, async move |this, cx| {
            let found = lookup.await;
            this.update_in(cx, |this, window, cx| {
                // The user may have asked for something else meanwhile, opened
                // another dialog, or removed the project and had its id given
                // to another.
                if this.runtime.read(cx).asked() != asked || window.has_active_dialog(cx) {
                    return;
                }
                match found {
                    Some(Ok((agents, in_git))) => {
                        this.show_new_chat_dialog(project, agents, in_git, window, cx)
                    }
                    Some(Err(err)) => window.push_notification(format!("{err:#}"), cx),
                    None => {}
                }
            })
            .ok();
        }));
    }

    fn show_new_chat_dialog(
        &mut self,
        project: store::Project,
        agents: Vec<Agent>,
        in_git: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let projects = self.layout.new_chat_projects(cx).then(|| {
            self.runtime
                .read(cx)
                .projects()
                .iter()
                .filter(|project| project.archived_at.is_none())
                .map(|project| (project.id, SharedString::from(project.name.clone())))
                .collect()
        });
        let args = NewChatArgs {
            path: project.path,
            agents,
            in_git,
            worktree_by_default: cx.global::<Settings>().worktree_by_default,
            projects,
            project: project.id,
        };
        let workspace = cx.weak_entity();
        dialogs::new_chat(
            window,
            cx,
            args,
            move |project, agent, title, worktree, _, cx| {
                workspace
                    .update(cx, |this, cx| {
                        this.apply(
                            Intent::NewChat {
                                project,
                                agent,
                                title,
                                worktree,
                            },
                            cx,
                        )
                    })
                    .ok();
            },
        );
    }
}
