//! The notification server's wire protocol: showing, closing and listening.

use std::collections::HashMap;

use smol::stream::StreamExt as _;
use zbus::zvariant::Value;

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
/// The bus itself, which says when the notification server changes.
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// The action invoked by clicking a notification's body.
pub const DEFAULT_ACTION: &str = "default";

/// The connection to the notification server. Notifications are sent on the
/// same connection their signals come in on: servers may send a click only
/// to the connection that sent the notification.
#[derive(Clone)]
pub struct Bus {
    connection: zbus::Connection,
}

/// What a notification says. The body is plain text.
#[derive(Debug, PartialEq, Eq)]
pub struct Note {
    pub summary: String,
    pub body: String,
    /// Whether it stays on screen until the user sees to it.
    pub urgent: bool,
}

#[derive(Debug)]
pub enum Signal {
    ActionInvoked(u32, String),
    ActivationToken(u32, String),
    Closed(u32),
}

impl Signal {
    /// The notification it's about.
    pub fn id(&self) -> u32 {
        match *self {
            Self::ActionInvoked(id, _) | Self::ActivationToken(id, _) | Self::Closed(id) => id,
        }
    }
}

/// What the connection to the notification server brings in, in the order
/// it came: a change of server, or a signal about one of the app's
/// notifications.
#[derive(Debug)]
pub enum Event {
    /// The server has changed: the unique name of the one there now, if any.
    Owner(Option<String>),
    /// A signal from the server there now.
    Signal(Signal),
}

/// Connects to the notification server and passes on what it says, for as
/// long as the session bus is there. Returns the connection and the server
/// there now, if any.
pub async fn listen(events: smol::channel::Sender<Event>) -> zbus::Result<(Bus, Option<String>)> {
    let connection = zbus::Connection::session().await?;
    // Every message the connection gets, replies included, in the order
    // they came. Made before the matches are added, so none is missed.
    let mut stream = zbus::MessageStream::from(&connection);
    let dbus = |method: &'static str, body: &str| {
        let connection = connection.clone();
        let body = body.to_owned();
        async move {
            connection
                .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), method, &(body,))
                .await
        }
    };
    // The bus passes on only the signals of the name's owner. They're
    // checked again as they come in, since anyone may send the app a
    // signal of their own.
    let signals = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(BUS_NAME)?
        .interface(BUS_NAME)?
        .path(OBJECT_PATH)?
        .build();
    let owners = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DBUS)?
        .interface(DBUS)?
        .member("NameOwnerChanged")?
        .arg(0, BUS_NAME)?
        .build();
    dbus("AddMatch", &signals.to_string()).await?;
    dbus("AddMatch", &owners.to_string()).await?;
    // It has no owner until a notification starts it.
    let owner = dbus("GetNameOwner", BUS_NAME)
        .await
        .and_then(|reply| reply.body().deserialize::<String>())
        .ok();
    let mut current = owner.clone();
    smol::spawn(async move {
        while let Some(message) = stream.next().await {
            let Ok(message) = message else {
                continue;
            };
            let Some(event) = event(&message, &mut current) else {
                continue;
            };
            if events.send(event).await.is_err() {
                break;
            }
        }
    })
    .detach();
    Ok((Bus { connection }, owner))
}

/// Sends `note` to the server at `destination`, or to the server there is
/// when it's `None` (a new notification), in place of notification
/// `replaces` unless it's 0. The notification goes to the server that
/// answered for what it can show.
pub async fn send(
    bus: &Bus,
    destination: Option<&str>,
    note: Note,
    replaces: u32,
) -> Result<u32, String> {
    let (server, markup) = capabilities(&bus.connection, destination.unwrap_or(BUS_NAME))
        .await
        .map_err(|err| err.to_string())?;
    let args = notify_args(note, replaces, true, markup);
    let reply = bus
        .connection
        .call_method(
            Some(server.as_str()),
            OBJECT_PATH,
            Some(BUS_NAME),
            "Notify",
            &args,
        )
        .await
        .map_err(|err| err.to_string())?;
    reply.body().deserialize().map_err(|err| err.to_string())
}

/// Takes notification `id` off the screen of the server `owner` that showed
/// it.
pub async fn close(bus: &Bus, owner: &str, id: u32) {
    bus.connection
        .call_method(
            Some(owner),
            OBJECT_PATH,
            Some(BUS_NAME),
            "CloseNotification",
            &(id,),
        )
        .await
        .ok();
}

/// Shows a notification that isn't tied to a chat, so the user can check
/// their notification settings.
pub async fn test(bus: &Bus, note: Note) -> Result<(), String> {
    let (server, markup) = capabilities(&bus.connection, BUS_NAME)
        .await
        .map_err(|err| err.to_string())?;
    let args = notify_args(note, 0, false, markup);
    bus.connection
        .call_method(
            Some(server.as_str()),
            OBJECT_PATH,
            Some(BUS_NAME),
            "Notify",
            &args,
        )
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

type NotifyArgs = (
    &'static str,
    u32,
    &'static str,
    String,
    String,
    Vec<&'static str>,
    HashMap<&'static str, Value<'static>>,
    i32,
);

/// The arguments for showing `note` in place of notification `replaces`
/// unless it's 0. `open` makes clicking it invoke the default action.
fn notify_args(note: Note, replaces: u32, open: bool, markup: bool) -> NotifyArgs {
    let body = if markup {
        escape(&note.body)
    } else {
        note.body
    };
    let actions = if open {
        vec![DEFAULT_ACTION, "Open"]
    } else {
        Vec::new()
    };
    let hints = HashMap::from([
        ("desktop-entry", Value::from("agentshub")),
        // The sound plays here, as its own setting asks.
        ("suppress-sound", Value::from(true)),
        ("urgency", Value::U8(if note.urgent { 2 } else { 1 })),
    ]);
    (
        "Agents Hub",
        replaces,
        "agentshub",
        note.summary,
        body,
        actions,
        hints,
        // The server's own timeout.
        -1,
    )
}

/// Escapes plain text for a body the server reads as markup, so a chat
/// titled `Fix <Button>` or a project named `R&D` shows as it's written.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Asks the server at `destination` what it can do. Returns the unique name
/// of the server that answered, which is where a notification written for
/// what it said must go, and whether it reads markup in the body.
async fn capabilities(
    connection: &zbus::Connection,
    destination: &str,
) -> zbus::Result<(String, bool)> {
    let asked = connection
        .call_method(
            Some(destination),
            OBJECT_PATH,
            Some(BUS_NAME),
            "GetCapabilities",
            &(),
        )
        .await;
    let (reply, markup) = match asked {
        Ok(reply) => {
            let capabilities: Vec<String> = reply.body().deserialize()?;
            let markup = capabilities.iter().any(|it| it == "body-markup");
            (reply, markup)
        }
        // A server that couldn't say gets escaped bodies, the safe guess
        // since most servers read markup. An error from the bus means
        // there's no server to send to.
        Err(zbus::Error::MethodError(_, _, reply))
            if reply
                .header()
                .sender()
                .is_some_and(|it| it.as_str() != DBUS) =>
        {
            (reply, true)
        }
        Err(err) => return Err(err),
    };
    let server = reply
        .header()
        .sender()
        .map(|it| it.to_string())
        .unwrap_or_else(|| destination.to_owned());
    Ok((server, markup))
}

/// What `message` tells the app, given the server's unique name `owner`,
/// which a change of server updates.
fn event(message: &zbus::Message, owner: &mut Option<String>) -> Option<Event> {
    use zbus::message::Type;

    let header = message.header();
    let sender = header.sender().map(|sender| sender.as_str());
    let from_owner = sender.is_some() && sender == owner.as_deref();
    if header.message_type() != Type::Signal {
        return None;
    }
    let interface = header.interface()?.as_str();
    let member = header.member()?.as_str();
    if sender == Some(DBUS) && interface == DBUS && member == "NameOwnerChanged" {
        let (name, _old, new): (String, String, String) = message.body().deserialize().ok()?;
        let new = (!new.is_empty()).then_some(new);
        if name != BUS_NAME || *owner == new {
            return None;
        }
        *owner = new.clone();
        return Some(Event::Owner(new));
    }
    if !from_owner || interface != BUS_NAME || header.path()?.as_str() != OBJECT_PATH {
        return None;
    }
    let body = message.body();
    let signal = match member {
        "ActionInvoked" => body
            .deserialize()
            .map(|(id, action)| Signal::ActionInvoked(id, action)),
        "ActivationToken" => body
            .deserialize()
            .map(|(id, token)| Signal::ActivationToken(id, token)),
        "NotificationClosed" => body
            .deserialize()
            .map(|(id, _reason): (u32, u32)| Signal::Closed(id)),
        _ => return None,
    };
    signal.ok().map(Event::Signal)
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_markup_in_bodies() {
        assert_eq!(escape("Fix <Button> · R&D"), "Fix &lt;Button&gt; · R&amp;D");
        assert_eq!(escape("plain"), "plain");
    }
}
