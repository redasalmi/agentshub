mod agent;
mod alerts;
mod appearance;
mod history;
mod instance;
mod palette;
mod reports;
#[cfg(target_os = "linux")]
mod scope;
mod sessions;
mod settings;
mod sidebar;
mod status;
mod store;
mod terminal;
mod terminal_view;
mod transcript;
mod tray;
mod workspace;
mod worktree;

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::FromRawFd as _;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;

use gpui_kit::*;

use crate::workspace::Workspace;

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
    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        terminal_view::init(cx);
        workspace::init(cx);
        // The tray decides whether closing the window quits.
        cx.set_quit_mode(QuitMode::Explicit);
        quit_on_sigterm(cx);
        // The window shows what is in the database, so it waits for it.
        cx.spawn(async move |cx| {
            let opened = store::open().await;
            cx.update(|cx| {
                let workspace = open_window(Contents::New(Box::new(opened)), None, cx);
                alerts::init(&workspace, cx);
                appearance::watch_desktop_motion(cx);
                tray::init(workspace, cx);
                instance::serve(instance, cx);
            });
        })
        .detach();
    });
}

/// Where the SIGTERM handler says that it ran.
static SIGTERM_PIPE: AtomicI32 = AtomicI32::new(-1);

/// Quits as the tray's Quit does on SIGTERM, as at logout or from
/// `systemctl`, so that every process started in Agents Hub is stopped and
/// what is queued is saved, instead of the app dying with them running.
fn quit_on_sigterm(cx: &mut App) {
    extern "C" fn on_sigterm(_: libc::c_int) {
        let byte = 1u8;
        // SAFETY: write is async-signal-safe, and `byte` outlives the call.
        unsafe {
            libc::write(
                SIGTERM_PIPE.load(Ordering::Relaxed),
                std::ptr::from_ref(&byte).cast(),
                1,
            )
        };
    }

    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        eprintln!("can't watch for SIGTERM: {}", io::Error::last_os_error());
        return;
    }
    let [read, write] = fds;
    // A handler that blocked on a full pipe would hang whatever thread it
    // interrupted.
    // SAFETY: fcntl takes no pointers.
    unsafe { libc::fcntl(write, libc::F_SETFL, libc::O_NONBLOCK) };
    SIGTERM_PIPE.store(write, Ordering::Relaxed);
    // SAFETY: sigaction is plain data, for which all zeroes is a valid value.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: `action` is valid for the call, and its handler only writes to
    // the pipe, which stays open.
    if unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) } != 0 {
        eprintln!("can't watch for SIGTERM: {}", io::Error::last_os_error());
        return;
    }
    // SAFETY: `read` is the pipe's read end, which nothing else owns.
    let mut pipe = unsafe { File::from_raw_fd(read) };
    let (quit, asked) = smol::channel::bounded(1);
    thread::spawn(move || {
        if pipe.read_exact(&mut [0]).is_ok() {
            quit.try_send(()).ok();
        }
    });
    cx.spawn(async move |cx| {
        if asked.recv().await.is_ok() {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

/// What a window opens on.
pub enum Contents {
    /// The workspace of the window that was closed to the tray.
    Workspace(Entity<Workspace>),
    /// A new workspace, on the database startup opened.
    New(Box<store::Opened>),
}

/// Opens the window on `contents`, with the `bounds` it was closed with, if
/// any.
pub fn open_window(
    contents: Contents,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) -> Entity<Workspace> {
    let options = WindowOptions {
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
        // The workspace draws the title bar, with the search in it.
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        window.on_window_should_close(cx, |window, cx| {
            tray::remember_bounds(window.window_bounds(), cx);
            true
        });
        // Each window observes the system's mode, as the observer goes with
        // the window when it's closed to the tray.
        window
            .observe_window_appearance(appearance::follow_system)
            .detach();
        let workspace = match contents {
            Contents::Workspace(workspace) => {
                workspace.update(cx, |workspace, cx| {
                    workspace.set_window(window);
                    workspace.set_hidden(false, cx);
                    workspace.focus_chat(window, cx);
                });
                workspace
            }
            Contents::New(opened) => {
                appearance::init(cx);
                let workspace = cx.new(|cx| Workspace::new(*opened, window, cx));
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
                if window.is_window_active() {
                    workspace.withdraw_seen(true, cx);
                } else {
                    workspace.release_modifiers(cx);
                }
            })
            .detach();
        });
        // The workspace loads the settings that pick the mode the first
        // time. Later, the system may have changed mode while the window was
        // closed, and the observer only fires on later changes.
        appearance::follow_system(window, cx);
        workspace
    })
    .expect("failed to open window");
    workspace
}
