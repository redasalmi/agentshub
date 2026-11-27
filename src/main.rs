mod alerts;
mod tray;

use std::borrow::Cow;

use agentshub_app::{bootstrap, settings};
#[cfg(target_os = "linux")]
use agentshub_platform::scope;
use agentshub_platform::sigterm::quit_on_sigterm;
use agentshub_platform::{instance, motion};
use agentshub_runtime::Runtime;
use agentshub_ui_kit::appearance;
use gpui_kit::*;

// The icons agentshub uses beyond the components' own.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Activity,
        Archive,
        ArchiveRestore,
        Clock,
        FolderCode,
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
            None => assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = assets::Assets.list(path)?;
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
    agentshub_runtime::terminal::set_scope(agentshub_runtime::terminal::Scope {
        pidfd: scope::pidfd,
        watch: scope::watch,
        available: scope::available,
        stop_all: scope::stop_all,
        wait_stopped: scope::wait_stopped,
        adopt: scope::adopt,
    });
    application().with_assets(Assets).run(move |cx| {
        init(cx);
        agentshub_app::editors::look_up(cx);
        agentshub_ui_kit::terminal_view::init(cx);
        agentshub_ui_kit::workspace::init(cx);
        // The tray decides whether closing the window quits.
        cx.set_quit_mode(QuitMode::Explicit);
        quit_on_sigterm(cx);
        // The window shows what is in the database, so it waits for it.
        cx.spawn(async move |cx| {
            let opened = bootstrap::open().await;
            cx.update(|cx| {
                let runtime = cx.new(|cx| Runtime::new(opened, cx));
                let frontend = agentshub_ui_classic::open(runtime.clone(), cx);
                alerts::init(&frontend, cx);
                motion::watch_desktop_motion(cx, appearance::apply_motion);
                tray::init(runtime, frontend, cx);
                cx.observe_global::<settings::Settings>(tray::sync).detach();
                instance::serve(instance, cx, tray::show);
            });
        })
        .detach();
    });
}
