//! Wiring notification policy to the workspace.

use std::rc::Rc;

use gpui_kit::*;

use crate::workspace::Workspace;

pub fn init(workspace: &Entity<Workspace>, cx: &mut App) {
    let workspace = workspace.downgrade();
    let open = Rc::new(move |chat: i64, token: Option<String>, cx: &mut App| {
        let Some(workspace) = workspace.upgrade() else {
            return;
        };
        crate::tray::show(token, cx);
        if let Some(window) = cx.windows().first() {
            window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| workspace.open_chat(chat, window, cx))
                })
                .ok();
        }
    });
    agentshub_app::alerts::start(open, cx);
}
