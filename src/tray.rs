//! Wiring the platform tray to the layout.

use std::rc::Rc;

use gpui_kit::*;

use agentshub_platform::tray as platform;

use agentshub_app::settings::Settings;
use agentshub_runtime::{Intent, Runtime};
use agentshub_ui_kit::Frontend;

pub(crate) fn init(runtime: Entity<Runtime>, frontend: Rc<dyn Frontend>, cx: &mut App) {
    let settings = Rc::clone(&frontend);
    let hidden = runtime;
    let enabled = cx.global::<Settings>().close_to_tray;
    platform::init(
        platform::Hooks {
            show: Box::new(move |_token, cx| {
                let bounds = platform::bounds(cx);
                frontend.reopen(bounds, cx);
            }),
            show_settings: Box::new(move |cx| settings.show_settings(cx)),
            set_hidden: Box::new(move |cx| {
                hidden.update(cx, |runtime, cx| runtime.apply(Intent::SetHidden(true), cx));
            }),
        },
        enabled,
        cx,
    );
}

/// Shows or removes the icon, as the settings ask.
pub(crate) fn sync(cx: &mut App) {
    let enabled = cx.global::<Settings>().close_to_tray;
    platform::sync(cx, enabled);
}

/// Brings the window back from the tray or another instance.
pub(crate) fn show(token: Option<String>, cx: &mut App) {
    platform::show(token, cx);
}
