mod alerts;
mod appearance;
mod palette;
mod sidebar;
mod terminal;
mod terminal_view;
mod tray;
mod window;
mod workspace;

pub use agentshub_app::{bootstrap, settings};
pub use agentshub_core::{
    agent, lifecycle, process, reports, sessions, status, store, transcript, worktree,
};
#[cfg(target_os = "linux")]
pub use agentshub_platform::scope;
pub use agentshub_platform::{instance, motion, sigterm};
pub use agentshub_ui::history;
pub use window::{Contents, open_window};

use std::borrow::Cow;

use gpui_kit::*;

use sigterm::quit_on_sigterm;

// The icons agentshub uses beyond the components' own.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Activity,
        Archive,
        ArchiveRestore,
        Clock,
        Pencil,
        Pin,
        PinOff,
        Settings
    ]
);

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
    let Some(instance) = instance::claim() else {
        return;
    };
    #[cfg(target_os = "linux")]
    if instance.owns_name() {
        std::thread::spawn(scope::stop_leftovers);
    }
    #[cfg(target_os = "linux")]
    agentshub_ui::terminal::set_scope(agentshub_ui::terminal::Scope {
        pidfd: scope::pidfd,
        watch: scope::watch,
        available: scope::available,
        stop_all: scope::stop_all,
        wait_stopped: scope::wait_stopped,
    });
    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        terminal_view::init(cx);
        workspace::init(cx);
        // The tray decides whether closing the window quits.
        cx.set_quit_mode(QuitMode::Explicit);
        quit_on_sigterm(cx);
        // The window shows what is in the database, so it waits for it.
        cx.spawn(async move |cx| {
            let opened = bootstrap::open().await;
            cx.update(|cx| {
                let workspace = open_window(Contents::New(Box::new(opened)), None, cx);
                alerts::init(&workspace, cx);
                motion::watch_desktop_motion(cx, appearance::apply_motion);
                tray::init(workspace, cx);
                cx.observe_global::<settings::Settings>(tray::sync).detach();
                instance::serve(instance, cx, tray::show);
            });
        })
        .detach();
    });
}
