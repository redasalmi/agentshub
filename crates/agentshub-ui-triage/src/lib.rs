//! The Triage layout: one list of every project's chats, grouped by what
//! needs the user, with a project filter at its top, which `TriageLayout`
//! lays out around the shared workspace.

use std::rc::Rc;

use gpui_kit::*;

use agentshub_runtime::Runtime;
use agentshub_ui_kit::{Frontend, frontend};
use layout::TriageLayout;

pub mod groups;
mod header;
pub mod layout;
mod list;

/// Opens the Triage layout's window on a new workspace on `runtime`.
pub fn open(runtime: Entity<Runtime>, cx: &mut App) -> Rc<dyn Frontend> {
    frontend::open(
        runtime.clone(),
        move |workspace, window, cx| Box::new(TriageLayout::new(runtime, workspace, window, cx)),
        cx,
    )
}
