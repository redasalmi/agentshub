//! Wiring the platform tray to the workspace.

use gpui_kit::*;

use agentshub_platform::tray as platform;

use crate::workspace::Workspace;
use agentshub_app::settings::Settings;
use agentshub_runtime::{Intent, Runtime};

pub use agentshub_platform::tray::remember_bounds;

pub fn init(runtime: Entity<Runtime>, workspace: Entity<Workspace>, cx: &mut App) {
    let (open_runtime, open) = (runtime.clone(), workspace.clone());
    let settings = workspace;
    let hidden = runtime;
    let enabled = cx.global::<Settings>().close_to_tray;
    platform::init(
        platform::Hooks {
            show: Box::new(move |_token, cx| {
                let bounds = platform::bounds(cx);
                crate::window::open_window(
                    open_runtime.clone(),
                    crate::window::Contents::Workspace(open.clone()),
                    bounds,
                    cx,
                );
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
                hidden.update(cx, |runtime, cx| runtime.apply(Intent::SetHidden(true), cx));
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
