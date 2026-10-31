//! The tray icon that keeps Agents Hub running after its window is closed,
//! while the settings ask for it. Without a tray to show it in, closing the
//! window quits.

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use ksni::TrayMethods as _;
use ksni::menu::StandardItem;

use crate::settings::Settings;
use crate::workspace::Workspace;

enum Event {
    /// Carries the tray's XDG activation token, which Wayland compositors
    /// need to let the window take focus from another app.
    Show(Option<String>),
    Quit,
    /// The desktop's tray came or went. While it's gone, the icon can't be
    /// seen, though the service behind it waits for it to come back.
    Available(bool),
}

struct Icon {
    events: smol::channel::Sender<Event>,
}

impl ksni::Tray for Icon {
    fn id(&self) -> String {
        "agentshub".into()
    }

    fn title(&self) -> String {
        "Agents Hub".into()
    }

    // shortcut: the icon comes from the installed icon theme, as the
    // .desktop file's does; embed a pixmap if it has to show uninstalled.
    fn icon_name(&self) -> String {
        "agentshub".into()
    }

    // shortcut: a token the host sends a few ms after the click (KDE can) is
    // lost, here and in the menu. Waiting in this callback can't catch it: it
    // blocks ksni's only D-Bus thread, so the token is read after it returns.
    // Fixing it needs ksni to wait for the token before calling back.
    fn activate(&mut self, _: i32, _: i32) {
        let token = ksni::current_xdg_activation_token();
        self.events.try_send(Event::Show(token)).ok();
    }

    fn watcher_online(&self) {
        self.events.try_send(Event::Available(true)).ok();
    }

    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.events.try_send(Event::Available(false)).ok();
        true
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show Agents Hub".into(),
                activate: Box::new(|this: &mut Self| {
                    let token = ksni::current_xdg_activation_token();
                    this.events.try_send(Event::Show(token)).ok();
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    this.events.try_send(Event::Quit).ok();
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

struct Tray {
    /// Held so the agents keep running while no window shows the workspace.
    workspace: Option<Entity<Workspace>>,
    handle: Option<ksni::Handle<Icon>>,
    /// Whether the desktop's tray shows the icon.
    available: bool,
    spawning: bool,
    events: smol::channel::Sender<Event>,
}

impl Global for Tray {}

pub fn init(workspace: Entity<Workspace>, cx: &mut App) {
    let (events, received) = smol::channel::unbounded();
    cx.set_global(Tray {
        workspace: Some(workspace),
        handle: None,
        available: false,
        spawning: false,
        events,
    });
    // The workspace shuts its agents down when it's released, which this
    // global would otherwise prevent.
    cx.on_app_quit(|cx| {
        cx.global_mut::<Tray>().workspace.take();
        async {}
    })
    .detach();
    cx.on_window_closed(|cx, _| {
        if !in_tray(cx) {
            cx.quit();
        } else if let Some(workspace) = cx.global::<Tray>().workspace.clone() {
            workspace.update(cx, |workspace, cx| workspace.set_hidden(true, cx));
        }
    })
    .detach();
    cx.observe_global::<Settings>(sync).detach();
    sync(cx);
    cx.spawn(async move |cx| {
        while let Ok(event) = received.recv().await {
            cx.update(|cx| match event {
                Event::Show(token) => show(token, cx),
                Event::Quit => cx.quit(),
                Event::Available(available) => {
                    cx.global_mut::<Tray>().available = available;
                    // Nothing else could bring a closed window back.
                    if !in_tray(cx) && cx.windows().is_empty() {
                        show(None, cx);
                    }
                }
            });
        }
    })
    .detach();
}

/// Whether the icon shows, so the app can be reached without a window.
fn in_tray(cx: &App) -> bool {
    let tray = cx.global::<Tray>();
    tray.available
        && tray
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_closed())
}

/// Shows or removes the icon, as the settings ask.
fn sync(cx: &mut App) {
    let enabled = cx.global::<Settings>().close_to_tray;
    let tray = cx.global_mut::<Tray>();
    if !enabled {
        if let Some(handle) = tray.handle.take() {
            drop(handle.shutdown());
        }
        return;
    }
    if tray.handle.is_some() || tray.spawning {
        return;
    }
    tray.spawning = true;
    // Set before the service starts, so the watcher going offline right after
    // registering isn't overwritten once the spawn completes.
    tray.available = true;
    let icon = Icon {
        events: tray.events.clone(),
    };
    cx.spawn(async move |cx| {
        let spawned = cx.background_spawn(icon.spawn()).await;
        cx.update(|cx| {
            cx.global_mut::<Tray>().spawning = false;
            match spawned {
                // The setting may have been turned off meanwhile.
                Ok(handle) if cx.global::<Settings>().close_to_tray => {
                    cx.global_mut::<Tray>().handle = Some(handle);
                }
                Ok(handle) => drop(handle.shutdown()),
                Err(err) => {
                    eprintln!("can't show the tray icon: {err}");
                    if let Some(window) = cx.windows().first() {
                        window
                            .update(cx, |_, window, cx| {
                                window.push_notification(
                                    format!(
                                        "Couldn't show the tray icon, so closing the window \
                                         quits Agents Hub: {err}"
                                    ),
                                    cx,
                                )
                            })
                            .ok();
                    }
                }
            }
        });
    })
    .detach();
}

/// Brings the window back, opening it again on the same workspace if it was
/// closed.
fn show(token: Option<String>, cx: &mut App) {
    if let Some(window) = cx.windows().first() {
        window
            .update(cx, |_, window, _| match token {
                Some(token) => {
                    if let Err(err) = activate_with_token(window, token) {
                        eprintln!("can't activate the window with the tray's token: {err}");
                        window.activate_window();
                    }
                }
                None => window.activate_window(),
            })
            .ok();
    } else if let Some(workspace) = cx.global::<Tray>().workspace.clone() {
        crate::open_window(Some(workspace), cx);
        if let (Some(token), Some(window)) = (token, cx.windows().first()) {
            window
                .update(cx, |_, window, _| activate_with_token(window, token))
                .ok();
        }
    }
}

/// Activates the window on Wayland with the token from the tray. GPUI has no
/// API for it, and its own request, made without the user's click, is likely
/// refused by compositors that prevent focus stealing.
fn activate_with_token(window: &Window, token: String) -> anyhow::Result<()> {
    use raw_window_handle::{
        HasDisplayHandle as _, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry::WlRegistry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
    use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

    struct State;
    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    delegate_noop!(State: ignore XdgActivationV1);

    let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) = (
        window.display_handle()?.as_raw(),
        HasWindowHandle::window_handle(window)?.as_raw(),
    ) else {
        anyhow::bail!("not on Wayland");
    };
    // SAFETY: GPUI's display and surface outlive this call, which only
    // borrows them on a queue of its own.
    let connection = Connection::from_backend(unsafe {
        Backend::from_foreign_display(display.display.as_ptr().cast())
    });
    let surface = WlSurface::from_id(&connection, unsafe {
        ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast())?
    })?;
    let (globals, queue) = registry_queue_init::<State>(&connection)?;
    let activation: XdgActivationV1 = globals.bind(&queue.handle(), 1..=1, ())?;
    activation.activate(token, &surface);
    activation.destroy();
    connection.flush()?;
    Ok(())
}
