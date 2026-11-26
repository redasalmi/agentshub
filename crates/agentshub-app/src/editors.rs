//! The code editors installed on this machine, looked up once at startup.

use gpui_kit::*;

use agentshub_core::editors::{self, Dirs, Found};

/// The installed editors on the built-in list, empty until they are looked up.
#[derive(Default)]
pub struct Installed(pub Vec<Found>);

impl Global for Installed {}

/// Looks up the installed editors off the foreground thread, and redraws once
/// they are known.
pub fn look_up(cx: &mut App) {
    cx.default_global::<Installed>();
    cx.spawn(async move |cx| {
        let found = cx
            .background_spawn(async move { editors::detect(&Dirs::from_env()) })
            .await;
        cx.update(|cx| {
            cx.set_global(Installed(found));
            cx.refresh_windows();
        });
    })
    .detach();
}
