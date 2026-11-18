//! The notification server's wire protocol: showing, closing and listening.

use super::*;

/// Sends `note`, written for a server that reads markup if `markup` says
/// so, to that `server`.
pub(super) fn notify(
    ticket: u64,
    note: Note,
    replaces: u32,
    bus: &Bus,
    server: &str,
    markup: bool,
    cx: &mut App,
) {
    let alerts = cx.global_mut::<Alerts>();
    let args = notify_args(note, replaces, true, markup);
    let message = zbus::Message::method_call(OBJECT_PATH, "Notify")
        .and_then(|it| it.destination(server))
        .and_then(|it| it.interface(BUS_NAME))
        .and_then(|it| it.build(&args))
        .map(|message| (bus.connection.clone(), message))
        .map_err(|err| err.to_string());
    let (connection, message) = match message {
        Ok(it) => it,
        Err(err) => {
            let effect = alerts.tracker.replied(ticket, Err(err));
            run(effect, cx);
            return;
        }
    };
    let serial = message.primary_header().serial_num();
    alerts.calls.insert(serial, ticket);
    cx.spawn(async move |cx| {
        let sent = cx
            .background_spawn(async move { connection.send(&message).await })
            .await;
        if let Err(err) = sent {
            cx.update(|cx| {
                receive(
                    Event::Reply {
                        serial,
                        id: Err(err.to_string()),
                    },
                    cx,
                )
            });
        }
    })
    .detach();
}

/// Takes notification `id` off the screen of the server that showed it.
pub(super) fn close(id: u32, cx: &mut App) {
    let alerts = cx.global::<Alerts>();
    let (Some(bus), Some(owner)) = (alerts.bus.clone(), alerts.tracker.owner.clone()) else {
        return;
    };
    cx.background_spawn(async move {
        bus.connection
            .call_method(
                Some(owner.as_str()),
                OBJECT_PATH,
                Some(BUS_NAME),
                "CloseNotification",
                &(id,),
            )
            .await
            .ok();
    })
    .detach();
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
pub(super) fn notify_args(note: Note, replaces: u32, open: bool, markup: bool) -> NotifyArgs {
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
pub(super) fn escape(text: &str) -> String {
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

/// Connects to the notification server and passes on what it says, for as
/// long as the session bus is there. Returns the connection and the server
/// there now, if any.
pub(super) async fn listen(
    events: smol::channel::Sender<Event>,
) -> zbus::Result<(Bus, Option<String>)> {
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

/// Asks the server at `destination` what it can do. Returns the unique name
/// of the server that answered, which is where a notification written for
/// what it said must go, and whether it reads markup in the body.
pub(super) async fn capabilities(
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
    match header.message_type() {
        Type::MethodReturn => Some(Event::Reply {
            serial: header.reply_serial()?,
            id: if from_owner {
                message.body().deserialize().map_err(|err| err.to_string())
            } else {
                Err("the notification server that showed it has gone".into())
            },
        }),
        Type::Error => Some(Event::Reply {
            serial: header.reply_serial()?,
            id: Err(zbus::Error::from(message.clone()).to_string()),
        }),
        Type::Signal => {
            let interface = header.interface()?.as_str();
            let member = header.member()?.as_str();
            if sender == Some(DBUS) && interface == DBUS && member == "NameOwnerChanged" {
                let (name, _old, new): (String, String, String) =
                    message.body().deserialize().ok()?;
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
        Type::MethodCall => None,
    }
}
