//! The Classic layout: the rail, the chat list, the chat header, the toolbar,
//! the footer and the workspace that lays them out.

use std::rc::Rc;

use gpui_kit::*;

use agentshub_runtime::Runtime;
use agentshub_ui_kit::Frontend;
use window::Contents;
use workspace::Workspace;

pub mod chat;
pub mod layout;
pub mod sidebar;
mod window;
pub mod workspace;

/// Opens the Classic layout's window on a new workspace on `runtime`.
pub fn open(runtime: Entity<Runtime>, cx: &mut App) -> Rc<dyn Frontend> {
    let workspace = window::open_window(runtime.clone(), Contents::New, None, cx);
    Rc::new(Classic { runtime, workspace })
}

struct Classic {
    runtime: Entity<Runtime>,
    workspace: Entity<Workspace>,
}

impl Frontend for Classic {
    fn reopen(&self, bounds: Option<WindowBounds>, cx: &mut App) {
        window::open_window(
            self.runtime.clone(),
            Contents::Workspace(self.workspace.clone()),
            bounds,
            cx,
        );
    }

    fn show_settings(&self, cx: &mut App) {
        let Some(window) = cx.windows().first().copied() else {
            return;
        };
        window
            .update(cx, |_, window, cx| {
                self.workspace.update(cx, |workspace, cx| {
                    workspace.show_settings(true, window, cx)
                })
            })
            .ok();
    }

    fn open_chat(&self, chat: i64, cx: &mut App) {
        if let Some(window) = cx.windows().first() {
            window
                .update(cx, |_, window, cx| {
                    self.workspace
                        .update(cx, |workspace, cx| workspace.open_chat(chat, window, cx))
                })
                .ok();
        }
    }
}
