//! Opening the app's window on a workspace.

use gpui_kit::*;

use crate::appearance;
use crate::tray;
use crate::workspace::Workspace;
use agentshub_runtime::Runtime;

/// What a window opens on.
pub enum Contents {
    /// The workspace of the window that was closed to the tray.
    Workspace(Entity<Workspace>),
    /// A new workspace, on the runtime built from the database startup
    /// opened.
    New(Entity<Runtime>),
}

/// Opens the window on `contents`, with the `bounds` it was closed with, if
/// any.
pub fn open_window(
    contents: Contents,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) -> Entity<Workspace> {
    let options = WindowOptions {
        window_bounds: Some(
            bounds.unwrap_or_else(|| WindowBounds::centered(size(px(1280.), px(800.)), cx)),
        ),
        titlebar: Some(TitlebarOptions {
            title: Some("Agents Hub".into()),
            ..Default::default()
        }),
        // Wayland has no per-window icon: the compositor looks up
        // agentshub.desktop by this id and uses its Icon=.
        app_id: Some("agentshub".into()),
        // The workspace draws the title bar, with the search in it.
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        window.on_window_should_close(cx, |window, cx| {
            tray::remember_bounds(window.window_bounds(), cx);
            true
        });
        // Each window observes the system's mode, as the observer goes with
        // the window when it's closed to the tray.
        window
            .observe_window_appearance(appearance::follow_system)
            .detach();
        let workspace = match contents {
            Contents::Workspace(workspace) => {
                workspace.update(cx, |workspace, cx| {
                    workspace.set_hidden(false, cx);
                    workspace.focus_chat(window, cx);
                });
                workspace
            }
            Contents::New(runtime) => {
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
        // The workspace loads the settings that pick the mode the first
        // time. Later, the system may have changed mode while the window was
        // closed, and the observer only fires on later changes.
        appearance::follow_system(window, cx);
        workspace
    })
    .expect("failed to open window");
    workspace
}
