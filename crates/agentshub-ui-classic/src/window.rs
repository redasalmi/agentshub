//! Opening the app's window on a workspace.

use gpui_kit::*;

use crate::workspace::Workspace;
use agentshub_runtime::{Intent, Runtime};
use agentshub_ui_kit::{appearance, frontend};

/// What a window opens on.
pub(crate) enum Contents {
    /// The workspace of the window that was closed to the tray.
    Workspace(Entity<Workspace>),
    /// A new workspace.
    New,
}

/// Opens the window on `contents`, a workspace on `runtime`, with the
/// `bounds` it was closed with, if any.
pub(crate) fn open_window(
    runtime: Entity<Runtime>,
    contents: Contents,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) -> Entity<Workspace> {
    let options = frontend::window_options(bounds, cx);
    let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        let workspace = match contents {
            Contents::Workspace(workspace) => {
                runtime.update(cx, |runtime, cx| {
                    runtime.apply(Intent::SetHidden(false), cx)
                });
                workspace.update(cx, |workspace, cx| workspace.focus_chat(window, cx));
                workspace
            }
            Contents::New => {
                appearance::init(cx);
                let workspace = cx.new(|cx| Workspace::new(runtime, window, cx));
                // The shortcuts reach the workspace through the focus.
                workspace.update(cx, |workspace, cx| workspace.focus_chat(window, cx));
                workspace
            }
        };
        // The header's spinner only runs while the window is on screen.
        // Redraw when that changes so it starts and stops with the window.
        workspace.update(cx, |_, cx| {
            cx.observe_window_visibility(window, |_, _, _, cx| cx.notify())
                .detach();
        });
        // The chat on screen may have had a notification while the window
        // was out of the user's sight.
        workspace.update(cx, |_, cx| {
            cx.observe_window_activation(window, |workspace, window, cx| {
                let active = window.is_window_active();
                workspace.set_focused(active, cx);
                if !active {
                    workspace.release_modifiers(cx);
                }
            })
            .detach();
        });
        frontend::watch_window(window, cx);
        workspace
    })
    .expect("failed to open window");
    workspace
}
