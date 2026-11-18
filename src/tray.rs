//! Wiring the platform tray to the workspace.

use gpui_kit::*;

use agentshub_platform::tray as platform;

use crate::settings::Settings;
use crate::workspace::Workspace;

pub use agentshub_platform::tray::remember_bounds;

pub fn init(workspace: Entity<Workspace>, cx: &mut App) {
    let open = workspace.clone();
    let settings = workspace.clone();
    let hidden = workspace.clone();
    let enabled = cx.global::<Settings>().close_to_tray;
    platform::init(
        platform::Hooks {
            show: Box::new(move |_token, cx| {
                let bounds = platform::bounds(cx);
                crate::open_window(crate::Contents::Workspace(open.clone()), bounds, cx);
            }),
            show_settings: Box::new(move |cx| {
                let Some(window) = cx.windows().first().copied() else {
                    return;
                };
                window
                    .update(cx, |_, window, cx| {
                        settings.update(cx, |workspace, cx| {
                            workspace.show_settings(true, window, cx)
                        })
                    })
                    .ok();
            }),
            set_hidden: Box::new(move |cx| {
                hidden.update(cx, |workspace, cx| workspace.set_hidden(true, cx));
            }),
        },
        enabled,
        cx,
    );
}

/// Shows or removes the icon, as the settings ask.
pub fn sync(cx: &mut App) {
    let enabled = cx.global::<Settings>().close_to_tray;
    platform::sync(cx, enabled);
}

/// Brings the window back from the tray or another instance.
pub fn show(token: Option<String>, cx: &mut App) {
    platform::show(token, cx);
}
