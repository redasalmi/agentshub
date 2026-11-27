//! How the binary opens a layout, and the window every layout opens in.

use std::rc::Rc;

use gpui_kit::*;

use crate::appearance;
use crate::layout::LayoutView;
use crate::workspace::Workspace;
use agentshub_platform::tray;
use agentshub_runtime::{Intent, Runtime};

/// What the tray, the notifications and other instances ask of the layout
/// the binary opened.
pub trait Frontend {
    /// Opens the window again on the layout it was closed to the tray with,
    /// at the `bounds` it was closed with, if any.
    fn reopen(&self, bounds: Option<WindowBounds>, cx: &mut App);
    /// Shows the settings in the window.
    fn show_settings(&self, cx: &mut App);
    /// Opens `chat` in the window.
    fn open_chat(&self, chat: i64, cx: &mut App);
}

/// Builds the layout of a new workspace, which it hears what to do from.
type BuildLayout = Box<
    dyn FnOnce(WeakEntity<Workspace>, &mut Window, &mut Context<Workspace>) -> Box<dyn LayoutView>,
>;

/// Opens the window on a new workspace on `runtime`, in the layout
/// `build_layout` builds.
pub fn open(
    runtime: Entity<Runtime>,
    build_layout: impl FnOnce(
        WeakEntity<Workspace>,
        &mut Window,
        &mut Context<Workspace>,
    ) -> Box<dyn LayoutView>
    + 'static,
    cx: &mut App,
) -> Rc<dyn Frontend> {
    let workspace = open_window(
        runtime.clone(),
        Contents::New(Box::new(build_layout)),
        None,
        cx,
    );
    Rc::new(Opened { runtime, workspace })
}

/// The window's workspace, which outlives the window closed to the tray.
struct Opened {
    runtime: Entity<Runtime>,
    workspace: Entity<Workspace>,
}

impl Frontend for Opened {
    fn reopen(&self, bounds: Option<WindowBounds>, cx: &mut App) {
        open_window(
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

/// What a window opens on.
enum Contents {
    /// The workspace of the window that was closed to the tray.
    Workspace(Entity<Workspace>),
    /// A new workspace, in the layout this builds.
    New(BuildLayout),
}

/// Opens the window on `contents`, a workspace on `runtime`, with the
/// `bounds` it was closed with, if any.
fn open_window(
    runtime: Entity<Runtime>,
    contents: Contents,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) -> Entity<Workspace> {
    let options = window_options(bounds, cx);
    let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        let workspace = match contents {
            Contents::Workspace(workspace) => {
                runtime.update(cx, |runtime, cx| {
                    runtime.apply(Intent::SetHidden(false), cx)
                });
                workspace.update(cx, |workspace, cx| workspace.focus_chat(window, cx));
                workspace
            }
            Contents::New(build_layout) => {
                appearance::init(cx);
                let workspace = cx.new(|cx| Workspace::new(runtime, build_layout, window, cx));
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
        watch_window(window, cx);
        workspace
    })
    .expect("failed to open window");
    workspace
}

/// The app's window, at the `bounds` it was closed with, if any.
pub fn window_options(bounds: Option<WindowBounds>, cx: &App) -> WindowOptions {
    WindowOptions {
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
        // The layout draws the title bar, with the search in it.
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    }
}

/// Remembers the window's bounds when it closes and follows the system's
/// mode. Called once the layout in the window is built.
pub fn watch_window(window: &mut Window, cx: &mut App) {
    window.on_window_should_close(cx, |window, cx| {
        tray::remember_bounds(window.window_bounds(), cx);
        true
    });
    // Each window observes the system's mode, as the observer goes with the
    // window when it's closed to the tray.
    window
        .observe_window_appearance(appearance::follow_system)
        .detach();
    // The layout loads the settings that pick the mode the first time.
    // Later, the system may have changed mode while the window was closed,
    // and the observer only fires on later changes.
    appearance::follow_system(window, cx);
}
