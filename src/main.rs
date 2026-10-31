mod agent;
mod appearance;
mod db;
mod history;
mod reports;
mod sessions;
mod settings;
mod sidebar;
mod status;
mod terminal;
mod terminal_view;
mod transcript;
mod tray;
mod workspace;

use std::borrow::Cow;

use gpui_kit::*;

use crate::workspace::Workspace;

// The icons agentshub uses beyond the components' own.
gpui_kit::assets::icon_assets!(ExtraIcons, [Pencil, Pin, PinOff, Settings]);

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

fn main() {
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        terminal_view::init(cx);
        workspace::init(cx);
        // The tray decides whether closing the window quits.
        cx.set_quit_mode(QuitMode::Explicit);
        let workspace = open_window(None, cx);
        tray::init(workspace, cx);
    });
}

/// Opens the window on `workspace`, or on a new one the first time.
pub fn open_window(workspace: Option<Entity<Workspace>>, cx: &mut App) -> Entity<Workspace> {
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
    let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        // Each window observes the system's mode, as the observer goes with
        // the window when it's closed to the tray.
        window
            .observe_window_appearance(appearance::follow_system)
            .detach();
        match workspace {
            Some(workspace) => {
                // The system may have changed mode while the window was
                // closed, and the observer only fires on later changes.
                appearance::follow_system(window, cx);
                workspace.update(cx, |workspace, cx| {
                    workspace.set_hidden(false, cx);
                    workspace.focus_chat(window, cx);
                });
                workspace
            }
            None => {
                appearance::init(window, cx);
                cx.new(|cx| Workspace::new(window, cx))
            }
        }
    })
    .expect("failed to open window");
    workspace
}
