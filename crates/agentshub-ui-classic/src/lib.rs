//! The Classic layout: the rail, the chat list, the chat header, the toolbar
//! and the footer, which `ClassicLayout` lays out around the shared
//! workspace.

use std::rc::Rc;

use gpui_kit::*;

use agentshub_runtime::Runtime;
use agentshub_ui_kit::{Frontend, frontend};
use layout::ClassicLayout;

pub mod chat;
pub mod layout;
pub mod sidebar;

/// Opens the Classic layout's window on a new workspace on `runtime`.
pub fn open(runtime: Entity<Runtime>, cx: &mut App) -> Rc<dyn Frontend> {
    frontend::open(
        runtime.clone(),
        move |workspace, window, cx| Box::new(ClassicLayout::new(runtime, workspace, window, cx)),
        cx,
    )
}
