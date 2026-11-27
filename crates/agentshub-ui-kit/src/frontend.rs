//! How the binary opens a layout, and the window every layout opens in.

use gpui_kit::*;

use crate::appearance;
use agentshub_platform::tray;

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
