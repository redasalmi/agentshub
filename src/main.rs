mod agent;
mod appearance;
mod db;
mod history;
mod reports;
mod sessions;
mod sidebar;
mod status;
mod terminal;
mod terminal_view;
mod transcript;
mod workspace;

use gpui_kit::*;

use crate::workspace::Workspace;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            terminal_view::init(cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Agents Hub".into()),
                    ..Default::default()
                }),
                // Wayland has no per-window icon: the compositor looks up
                // agentshub.desktop by this id and uses its Icon=.
                app_id: Some("agentshub".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                appearance::init(window, cx);
                cx.new(|cx| Workspace::new(window, cx))
            })
            .expect("failed to open window");
        });
}
