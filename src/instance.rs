//! Keeps Agents Hub to one instance on the session bus: the first serves
//! `org.freedesktop.Application`, and launching it again activates that one.

use std::collections::HashMap;

use gpui_kit::*;
use zbus::fdo::RequestNameFlags;
use zbus::zvariant::{OwnedValue, Value};

const BUS_NAME: &str = "io.github.redasalmi.agentshub";
const OBJECT_PATH: &str = "/io/github/redasalmi/agentshub";
const INTERFACE: &str = "org.freedesktop.Application";

struct Application {
    /// Carries the launcher's XDG activation token, which Wayland compositors
    /// need to let the window take focus.
    shown: smol::channel::Sender<Option<String>>,
}

#[zbus::interface(name = "org.freedesktop.Application")]
impl Application {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        let token = platform_data
            .get("activation-token")
            .and_then(|token| String::try_from(token.clone()).ok());
        self.shown.try_send(token).ok();
    }
}

/// The name this instance holds on the session bus, and the launches that
/// asked it to show its window since.
pub struct Instance {
    connection: Option<zbus::blocking::Connection>,
    shown: smol::channel::Receiver<Option<String>>,
}

/// Takes the name on the session bus before anything else starts, so of two
/// launches only one goes on. Returns `None` when another instance has it and
/// was asked to show its window instead.
pub fn claim() -> Option<Instance> {
    let (sender, shown) = smol::channel::unbounded();
    let connection = match own(sender) {
        Ok(connection) => Some(connection),
        Err(zbus::Error::NameTaken) => match activate_owner() {
            Ok(()) => return None,
            Err(err) => {
                eprintln!("can't reach the running Agents Hub: {err}");
                None
            }
        },
        Err(err) => {
            eprintln!("can't take {BUS_NAME} on the session bus: {err}");
            None
        }
    };
    Some(Instance { connection, shown })
}

fn own(shown: smol::channel::Sender<Option<String>>) -> zbus::Result<zbus::blocking::Connection> {
    let connection = zbus::blocking::connection::Builder::session()?
        .serve_at(OBJECT_PATH, Application { shown })?
        .build()?;
    // Only this flag: zbus's default flags would let a later launch take the
    // name from this one.
    connection.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())?;
    Ok(connection)
}

/// Asks the instance holding the name to show its window, passing on the
/// token the launcher gave this one.
fn activate_owner() -> zbus::Result<()> {
    let mut platform_data = HashMap::new();
    for (key, var) in [
        ("activation-token", "XDG_ACTIVATION_TOKEN"),
        ("desktop-startup-id", "DESKTOP_STARTUP_ID"),
    ] {
        if let Ok(value) = std::env::var(var) {
            platform_data.insert(key, Value::from(value));
        }
    }
    zbus::blocking::Connection::session()?.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(INTERFACE),
        "Activate",
        &(platform_data,),
    )?;
    Ok(())
}

/// Shows the window each time a later launch asks for it, including those
/// that asked while the app was starting.
pub fn serve(instance: Instance, cx: &mut App) {
    cx.spawn(async move |cx| {
        // Taken whole, so the task keeps the connection, and with it the
        // name, for as long as the app runs.
        let Instance {
            connection: _connection,
            shown,
        } = instance;
        while let Ok(token) = shown.recv().await {
            cx.update(|cx| crate::tray::show(token, cx));
        }
    })
    .detach();
}
