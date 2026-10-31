//! Tells the user when an agent finishes or needs them while they're looking
//! elsewhere: with a desktop notification, a sound, or both, as the settings
//! ask.
//!
//! Notifications go to the desktop's notification server, which shows them
//! the way the user set it up. The sound is played here, so it plays without
//! a notification too, and the same on desktops whose server plays none.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::num::{NonZero, NonZeroU32};
use std::time::{Duration, Instant};

use gpui_kit::*;
use smol::stream::StreamExt as _;
use zbus::zvariant::Value;

use crate::agent::Agent;
use crate::settings::Settings;
use crate::status::Status;
use crate::workspace::Workspace;

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
/// The bus itself, which says when the notification server changes.
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// The action invoked by clicking a notification's body.
const DEFAULT_ACTION: &str = "default";

/// Sounds closer together than this play once, so agents finishing together
/// don't make a racket.
const SOUND_GAP: Duration = Duration::from_millis(1500);

/// What happened to an agent that the user should hear about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    Finished,
    /// Can't go on without the user.
    NeedsYou,
    Failed,
}

impl Alert {
    /// What going from `old` to `new` tells the user, if anything. Reports
    /// that repeat the status tell them nothing new.
    pub fn of(agent: Agent, old: Option<Status>, new: Option<Status>) -> Option<Self> {
        if old == new {
            return None;
        }
        match new? {
            Status::Blocked(_) => Some(Self::NeedsYou),
            Status::Done => Some(Self::Finished),
            Status::Error => Some(Self::Failed),
            // Codex goes back to idle without saying it's done. The others
            // go back to idle when a turn is cancelled, which the user did.
            Status::Idle
                if agent == Agent::Codex
                    && matches!(old, Some(Status::Working | Status::Blocked(_))) =>
            {
                Some(Self::Finished)
            }
            Status::Idle | Status::Working => None,
        }
    }
}

/// A click on a notification, with the token that lets the window take the
/// focus on Wayland, when the server sent one.
#[derive(Debug, PartialEq, Eq)]
struct Click {
    chat: i64,
    token: Option<String>,
}

#[derive(Debug)]
enum Signal {
    ActionInvoked(u32, String),
    ActivationToken(u32, String),
    Closed(u32),
}

impl Signal {
    /// The notification it's about.
    fn id(&self) -> u32 {
        match *self {
            Self::ActionInvoked(id, _) | Self::ActivationToken(id, _) | Self::Closed(id) => id,
        }
    }
}

/// What the connection to the notification server brings in, in the order
/// it came: a reply and the signals that follow it about the notification
/// it names must be seen to in that order, as must a change of server and
/// what each server says.
#[derive(Debug)]
enum Event {
    /// The server has changed: the unique name of the one there now, if any.
    Owner(Option<String>),
    /// A signal from the server there now.
    Signal(Signal),
    /// The reply to the call with this serial: the id of the notification
    /// it showed, or why it showed none. A notification shown by a server
    /// that has since gone counts as none.
    Reply {
        serial: NonZeroU32,
        id: Result<u32, String>,
    },
}

/// The connection to the notification server. Notifications are sent on the
/// same connection their signals come in on: servers may send a click only
/// to the connection that sent the notification.
#[derive(Clone)]
struct Bus {
    connection: zbus::Connection,
}

/// What a notification says. The body is plain text.
#[derive(Debug, PartialEq, Eq)]
struct Note {
    summary: String,
    body: String,
}

/// Where a chat's notification is at.
#[derive(Default)]
struct Shown {
    /// The server's id for the notification on screen.
    id: Option<u32>,
    /// Whether the chat still has something to tell, which it hasn't once
    /// the user has seen to it. A click on its notification then opens
    /// nothing, and the one on its way is closed once it's shown.
    wanted: bool,
    /// The notification on its way to the server, by its ticket. One is at
    /// a time, so each knows the id of the one it replaces, and the one on
    /// screen is closed only once nothing is on its way to replace it.
    sending: Option<u64>,
    /// The latest notification asked for while another was on its way.
    queued: Option<Note>,
}

impl Shown {
    fn idle(&self) -> bool {
        self.id.is_none() && self.sending.is_none() && self.queued.is_none()
    }
}

/// What the tracker needs done about the server.
#[derive(Debug, PartialEq, Eq)]
enum Effect {
    /// Show `note` in place of notification `replaces` unless it's 0, and
    /// tell the tracker how it went with `ticket`.
    Send {
        ticket: u64,
        note: Note,
        replaces: u32,
    },
    /// Take notification `id` off the screen.
    Close(u32),
    /// Bring the window back on the chat that was clicked.
    Open(Click),
}

/// Keeps track of each chat's notification, apart from the bus so that what
/// happens in what order can be tested.
#[derive(Default)]
struct Tracker {
    /// The unique name of the server that the ids are from.
    owner: Option<String>,
    /// The notification of each chat that has or is getting one. A chat's
    /// next one replaces it, and it's taken back once the chat is looked at.
    chats: HashMap<i64, Shown>,
    /// The tokens the server sent ahead of a click, by notification.
    tokens: HashMap<u32, String>,
    /// The last ticket given to a notification on its way.
    ticket: u64,
}

impl Tracker {
    /// Whether there's anything about `chat` to take back.
    fn tracks(&self, chat: i64) -> bool {
        self.chats.contains_key(&chat)
    }

    /// The chat whose notification on screen the server knows as `id`.
    fn chat_of(&self, id: u32) -> Option<i64> {
        self.chats
            .iter()
            .find(|(_, shown)| shown.id == Some(id))
            .map(|(chat, _)| *chat)
    }

    /// Forgets `chat`'s notification once there's nothing left to track.
    fn tidy(&mut self, chat: i64) {
        if self.chats.get(&chat).is_some_and(Shown::idle) {
            self.chats.remove(&chat);
        }
    }

    /// Sends `note` about `chat`, in place of the notification it has.
    fn start(&mut self, chat: i64, note: Note) -> Effect {
        self.ticket += 1;
        let shown = self.chats.entry(chat).or_default();
        shown.sending = Some(self.ticket);
        Effect::Send {
            ticket: self.ticket,
            note,
            replaces: shown.id.unwrap_or(0),
        }
    }

    /// Shows `note` about `chat`, in place of the notification it has. Waits
    /// for one still on its way, so that each knows the one it replaces.
    fn show(&mut self, chat: i64, note: Note) -> Option<Effect> {
        let shown = self.chats.entry(chat).or_default();
        shown.wanted = true;
        if shown.sending.is_some() {
            shown.queued = Some(note);
            None
        } else {
            Some(self.start(chat, note))
        }
    }

    /// Takes back the notification about `chat`, along with any still on
    /// its way.
    fn withdraw(&mut self, chat: i64) -> Option<Effect> {
        let shown = self.chats.get_mut(&chat)?;
        shown.wanted = false;
        shown.queued = None;
        // The one on its way replaces the one on screen, and is closed once
        // it's shown. Closing the one on screen now could close what's asked
        // for after this, which may take its place first.
        if shown.sending.is_some() {
            return None;
        }
        let id = shown.id.take();
        self.tidy(chat);
        let id = id?;
        self.tokens.remove(&id);
        Some(Effect::Close(id))
    }

    /// Keeps track of the notification that went with `ticket`, then sends
    /// what was asked for since or closes it when it was taken back.
    fn replied(&mut self, ticket: u64, id: Result<u32, String>) -> Option<Effect> {
        let (&chat, shown) = self
            .chats
            .iter_mut()
            .find(|(_, shown)| shown.sending == Some(ticket))?;
        shown.sending = None;
        match id {
            Ok(id) => shown.id = Some(id),
            // The one it was to replace stays.
            Err(err) => eprintln!("can't show a notification: {err}"),
        }
        let effect = if let Some(next) = shown.queued.take() {
            Some(self.start(chat, next))
        } else if !shown.wanted
            && let Some(id) = shown.id.take()
        {
            self.tokens.remove(&id);
            Some(Effect::Close(id))
        } else {
            None
        };
        self.tidy(chat);
        effect
    }

    /// Forgets the ids of the server that has gone: the new one gives its
    /// own, which may be the same numbers for other notifications. The
    /// notifications on their way are kept track of, since their replies
    /// say which server showed them.
    fn owner_changed(&mut self, owner: Option<String>) {
        if self.owner == owner {
            return;
        }
        self.owner = owner;
        self.tokens.clear();
        for shown in self.chats.values_mut() {
            shown.id = None;
        }
        self.chats.retain(|_, shown| !shown.idle());
    }

    /// Keeps track of the server's signals about the app's notifications,
    /// and says which chat a click is for.
    fn signal(&mut self, signal: Signal) -> Option<Effect> {
        let chat = self.chat_of(signal.id())?;
        match signal {
            // Sent just before the click it goes with.
            Signal::ActivationToken(id, token) => {
                self.tokens.insert(id, token);
                None
            }
            Signal::ActionInvoked(id, action) => {
                let token = self.tokens.remove(&id);
                let wanted = self.chats.get(&chat).is_some_and(|shown| shown.wanted);
                (wanted && action == DEFAULT_ACTION).then_some(Effect::Open(Click { chat, token }))
            }
            Signal::Closed(id) => {
                self.tokens.remove(&id);
                if let Some(shown) = self.chats.get_mut(&chat) {
                    shown.id = None;
                }
                self.tidy(chat);
                None
            }
        }
    }
}

struct Alerts {
    workspace: WeakEntity<Workspace>,
    /// The connection to the server, once it's open.
    bus: Option<Bus>,
    tracker: Tracker,
    /// The ticket of each notification whose reply hasn't come, by the
    /// serial of its call.
    calls: HashMap<NonZeroU32, u64>,
    last_sound: Option<Instant>,
}

impl Global for Alerts {}

pub fn init(workspace: &Entity<Workspace>, cx: &mut App) {
    cx.set_global(Alerts {
        workspace: workspace.downgrade(),
        bus: None,
        tracker: Tracker::default(),
        calls: HashMap::new(),
        last_sound: None,
    });

    let (events, received) = smol::channel::unbounded();
    cx.spawn(async move |cx| {
        let (bus, owner) = match cx.background_spawn(listen(events)).await {
            Ok(it) => it,
            Err(err) => {
                eprintln!("can't listen to the notification server: {err}");
                return;
            }
        };
        cx.update(|cx| {
            let alerts = cx.global_mut::<Alerts>();
            alerts.bus = Some(bus);
            alerts.tracker.owner = owner;
        });
        while let Ok(event) = received.recv().await {
            cx.update(|cx| receive(event, cx));
        }
    })
    .detach();
}

/// Lets the user know `chat`'s agent did what `status` says, which made the
/// `kind` of alert given. `body` says which chat it is.
pub fn alert(chat: i64, agent: Agent, kind: Alert, status: Status, body: String, cx: &mut App) {
    let settings = *cx.global::<Settings>();
    if settings.notification_sound {
        play(kind, cx);
    }
    if settings.notification_popups {
        // Codex says it's finished by going back to idle.
        let status = match status {
            Status::Idle => Status::Done,
            status => status,
        };
        let summary = crate::sidebar::activity(agent, Some(Some(status)));
        show(chat, Note { summary, body }, cx);
    }
}

/// Takes back the notification about `chat`, which the user has seen to or
/// deleted, along with any still on its way.
pub fn withdraw(chat: i64, cx: &mut App) {
    if !cx
        .try_global::<Alerts>()
        .is_some_and(|alerts| alerts.tracker.tracks(chat))
    {
        return;
    }
    let effect = cx.global_mut::<Alerts>().tracker.withdraw(chat);
    run(effect, cx);
}

/// Shows what the settings ask for when something happens, so the user can
/// hear and see it without waiting for an agent.
pub fn test(cx: &mut App) {
    let settings = *cx.global::<Settings>();
    if settings.notification_sound {
        cx.global_mut::<Alerts>().last_sound = None;
        play(Alert::Finished, cx);
    }
    if settings.notification_popups {
        let Some(bus) = cx.global::<Alerts>().bus.clone() else {
            eprintln!("can't show a notification: no notification server");
            return;
        };
        let note = Note {
            summary: "Notifications are on".into(),
            body: "Agents Hub tells you like this when an agent finishes or needs you.".into(),
        };
        cx.background_spawn(async move {
            let shown = async {
                let (server, markup) = capabilities(&bus.connection, BUS_NAME).await?;
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
            };
            if let Err(err) = shown.await {
                eprintln!("can't show a notification: {err}");
            }
        })
        .detach();
    }
}

fn show(chat: i64, note: Note, cx: &mut App) {
    let alerts = cx.global_mut::<Alerts>();
    if alerts.bus.is_none() {
        eprintln!("can't show a notification: no notification server");
        return;
    }
    let effect = alerts.tracker.show(chat, note);
    run(effect, cx);
}

/// Sees to what comes in from the server.
fn receive(event: Event, cx: &mut App) {
    let alerts = cx.global_mut::<Alerts>();
    let effect = match event {
        Event::Owner(owner) => {
            alerts.tracker.owner_changed(owner);
            None
        }
        Event::Signal(signal) => alerts.tracker.signal(signal),
        Event::Reply { serial, id } => alerts
            .calls
            .remove(&serial)
            .and_then(|ticket| alerts.tracker.replied(ticket, id)),
    };
    run(effect, cx);
}

/// Does what the tracker needs done.
fn run(effect: Option<Effect>, cx: &mut App) {
    match effect {
        Some(Effect::Send {
            ticket,
            note,
            replaces,
        }) => send(ticket, note, replaces, cx),
        Some(Effect::Close(id)) => close(id, cx),
        Some(Effect::Open(click)) => open(click, cx),
        None => {}
    }
}

/// Sends `note` to the server, in place of notification `replaces` unless
/// it's 0. Its reply comes in with the server's other messages.
fn send(ticket: u64, note: Note, replaces: u32, cx: &mut App) {
    let alerts = cx.global_mut::<Alerts>();
    // An id means something only to the server that gave it, so a
    // replacement goes to that server alone. A new one goes to whichever
    // server there is, which the bus starts if need be.
    let destination = if replaces == 0 {
        Some(BUS_NAME.to_owned())
    } else {
        alerts.tracker.owner.clone()
    };
    let (Some(bus), Some(destination)) = (alerts.bus.clone(), destination) else {
        let effect = alerts
            .tracker
            .replied(ticket, Err("no notification server".into()));
        run(effect, cx);
        return;
    };
    // Whether the body is escaped depends on the server, which may not be
    // the one there was when the last notification was sent, so it's asked
    // each time, and the notification goes to the server that answered.
    cx.spawn(async move |cx| {
        let connection = bus.connection.clone();
        let asked = cx
            .background_spawn(async move { capabilities(&connection, &destination).await })
            .await;
        cx.update(|cx| match asked {
            Ok((server, markup)) => notify(ticket, note, replaces, &bus, &server, markup, cx),
            Err(err) => {
                let effect = cx
                    .global_mut::<Alerts>()
                    .tracker
                    .replied(ticket, Err(err.to_string()));
                run(effect, cx);
            }
        });
    })
    .detach();
}

/// Sends `note`, written for a server that reads markup if `markup` says
/// so, to that `server`.
fn notify(
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
fn close(id: u32, cx: &mut App) {
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

/// Brings the window back on the chat that was clicked.
fn open(click: Click, cx: &mut App) {
    let Some(workspace) = cx.global::<Alerts>().workspace.upgrade() else {
        return;
    };
    crate::tray::show(click.token, cx);
    if let Some(window) = cx.windows().first() {
        window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_chat(click.chat, window, cx)
                })
            })
            .ok();
    }
}

/// Connects to the notification server and passes on what it says, for as
/// long as the session bus is there. Returns the connection and the server
/// there now, if any.
async fn listen(events: smol::channel::Sender<Event>) -> zbus::Result<(Bus, Option<String>)> {
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

fn play(kind: Alert, cx: &mut App) {
    let alerts = cx.global_mut::<Alerts>();
    let now = Instant::now();
    if alerts
        .last_sound
        .is_some_and(|last| now.duration_since(last) < SOUND_GAP)
    {
        return;
    }
    alerts.last_sound = Some(now);
    // Opening the device and waiting for the sound to end both block.
    let spawned = std::thread::Builder::new()
        .name("alert-sound".into())
        .spawn(move || {
            if let Err(err) = play_blocking(kind) {
                eprintln!("can't play the notification sound: {err}");
            }
        });
    if let Err(err) = spawned {
        eprintln!("can't play the notification sound: {err}");
    }
}

fn play_blocking(kind: Alert) -> anyhow::Result<()> {
    let mut sink = rodio::DeviceSinkBuilder::open_default_sink()?;
    sink.log_on_drop(false);
    let player = rodio::Player::connect_new(sink.mixer());
    player.append(rodio::buffer::SamplesBuffer::new(
        NonZero::new(1).unwrap(),
        NonZero::new(SAMPLE_RATE).unwrap(),
        chime(kind),
    ));
    player.sleep_until_end();
    // What the device still holds plays out before it's closed.
    std::thread::sleep(Duration::from_millis(150));
    Ok(())
}

const SAMPLE_RATE: u32 = 48_000;

/// The notes of each sound: their pitch in Hz and when they start, in
/// seconds. Finishing rises, needing the user knocks, failing falls.
fn notes(kind: Alert) -> &'static [(f32, f32)] {
    match kind {
        Alert::Finished => &[(659.25, 0.0), (880.0, 0.11)],
        Alert::NeedsYou => &[(987.77, 0.0), (987.77, 0.13), (1318.51, 0.26)],
        Alert::Failed => &[(880.0, 0.0), (587.33, 0.14)],
    }
}

/// A soft bell-like sound, made here so no sound files ship with the app.
fn chime(kind: Alert) -> Vec<f32> {
    const NOTE_LEN: f32 = 0.7;
    const ATTACK: f32 = 0.004;
    const DECAY: f32 = 0.16;
    const PEAK: f32 = 0.3;

    let notes = notes(kind);
    let end = notes.iter().map(|&(_, start)| start).fold(0.0, f32::max) + NOTE_LEN;
    let rate = SAMPLE_RATE as f32;
    let mut samples = vec![0.0f32; (end * rate) as usize];
    for &(pitch, start) in notes {
        let first = (start * rate) as usize;
        for (i, sample) in samples[first..].iter_mut().enumerate() {
            let t = i as f32 / rate;
            if t >= NOTE_LEN {
                break;
            }
            let envelope = (t / ATTACK).min(1.0) * (-t / DECAY).exp();
            // A few overtones, fading faster than the note, make it ring.
            let tone = (TAU * pitch * t).sin()
                + 0.25 * (TAU * 2.0 * pitch * t).sin() * (-t / 0.05).exp()
                + 0.1 * (TAU * 3.0 * pitch * t).sin() * (-t / 0.03).exp();
            *sample += envelope * tone;
        }
    }
    let loudest = samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
    if loudest > 0.0 {
        for sample in &mut samples {
            *sample *= PEAK / loudest;
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    // Not a glob: GPUI's `test` attribute would take the place of the
    // standard one.
    use super::{Alert, Click, Effect, Note, Signal, Tracker, chime, escape};
    use crate::agent::Agent;
    use crate::status::{Need, Status};

    fn note(text: &str) -> Note {
        Note {
            summary: text.into(),
            body: text.into(),
        }
    }

    /// Shows `text` about `chat` and returns the ticket it was sent with,
    /// and the id it replaces.
    fn send(tracker: &mut Tracker, chat: i64, text: &str) -> (u64, u32) {
        match tracker.show(chat, note(text)) {
            Some(Effect::Send {
                ticket, replaces, ..
            }) => (ticket, replaces),
            effect => panic!("expected a send, got {effect:?}"),
        }
    }

    fn tracker() -> Tracker {
        Tracker {
            owner: Some(":1.1".into()),
            ..Tracker::default()
        }
    }

    #[test]
    fn clicks_open_the_chat_with_its_token() {
        let mut tracker = tracker();
        let (ticket, replaces) = send(&mut tracker, 7, "a");
        assert_eq!(replaces, 0);
        assert_eq!(tracker.replied(ticket, Ok(3)), None);
        assert_eq!(
            tracker.signal(Signal::ActivationToken(3, "token".into())),
            None
        );
        assert_eq!(
            tracker.signal(Signal::ActionInvoked(3, "default".into())),
            Some(Effect::Open(Click {
                chat: 7,
                token: Some("token".into())
            }))
        );
        // Not the app's.
        assert_eq!(
            tracker.signal(Signal::ActionInvoked(4, "default".into())),
            None
        );
    }

    #[test]
    fn replaces_the_notification_on_screen() {
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        let (ticket, replaces) = send(&mut tracker, 7, "b");
        assert_eq!(replaces, 3);
        // The latest asked for while it's on its way replaces it.
        assert_eq!(tracker.show(7, note("c")), None);
        assert_eq!(tracker.show(7, note("d")), None);
        match tracker.replied(ticket, Ok(3)) {
            Some(Effect::Send { note, replaces, .. }) => {
                assert_eq!(note.summary, "d");
                assert_eq!(replaces, 3);
            }
            effect => panic!("expected a send, got {effect:?}"),
        }
    }

    #[test]
    fn closes_only_once_nothing_is_on_its_way() {
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        let (ticket, _) = send(&mut tracker, 7, "b");
        // Taken back while the replacement is on its way, then asked for
        // again: closing 3 now could close the newest.
        assert_eq!(tracker.withdraw(7), None);
        assert_eq!(tracker.show(7, note("c")), None);
        assert!(matches!(
            tracker.replied(ticket, Ok(3)),
            Some(Effect::Send { replaces: 3, .. })
        ));
    }

    #[test]
    fn closes_what_was_taken_back_on_its_way() {
        // Nothing was asked for since: it's closed once shown, and a click
        // on it meanwhile opens nothing.
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        let (ticket, _) = send(&mut tracker, 7, "b");
        assert_eq!(tracker.withdraw(7), None);
        assert_eq!(
            tracker.signal(Signal::ActionInvoked(3, "default".into())),
            None
        );
        assert_eq!(tracker.replied(ticket, Ok(3)), Some(Effect::Close(3)));
        assert!(!tracker.tracks(7));
    }

    #[test]
    fn closes_what_failed_to_replace() {
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        let (ticket, _) = send(&mut tracker, 7, "b");
        tracker.withdraw(7);
        assert_eq!(
            tracker.replied(ticket, Err("no".into())),
            Some(Effect::Close(3))
        );
    }

    #[test]
    fn withdraws_what_is_shown() {
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        tracker.signal(Signal::ActivationToken(3, "token".into()));
        assert_eq!(tracker.withdraw(7), Some(Effect::Close(3)));
        assert!(!tracker.tracks(7));
        assert!(tracker.tokens.is_empty());
        assert_eq!(tracker.withdraw(7), None);
    }

    #[test]
    fn forgets_notifications_the_server_closed() {
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        assert_eq!(tracker.signal(Signal::Closed(3)), None);
        assert!(!tracker.tracks(7));
        let (_, replaces) = send(&mut tracker, 7, "b");
        assert_eq!(replaces, 0);
    }

    #[test]
    fn forgets_ids_when_the_server_changes() {
        let mut tracker = tracker();
        let (ticket, _) = send(&mut tracker, 7, "a");
        tracker.replied(ticket, Ok(3));
        tracker.signal(Signal::ActivationToken(3, "token".into()));
        let (pending, _) = send(&mut tracker, 8, "b");

        tracker.owner_changed(Some(":1.2".into()));
        assert!(!tracker.tracks(7));
        assert!(tracker.tokens.is_empty());
        // The new server's 3 is someone else's.
        assert_eq!(
            tracker.signal(Signal::ActionInvoked(3, "default".into())),
            None
        );
        // The old server's reply comes in as a failure, and leaves nothing.
        assert_eq!(tracker.replied(pending, Err("gone".into())), None);
        assert!(!tracker.tracks(8));
        let (_, replaces) = send(&mut tracker, 7, "c");
        assert_eq!(replaces, 0);
    }

    #[test]
    fn alerts_on_what_needs_the_user() {
        let working = Some(Status::Working);
        let of = |old, new| Alert::of(Agent::Claude, old, new);
        assert_eq!(of(working, Some(Status::Done)), Some(Alert::Finished));
        assert_eq!(
            of(working, Some(Status::Blocked(Some(Need::Permission)))),
            Some(Alert::NeedsYou)
        );
        assert_eq!(
            of(
                Some(Status::Blocked(Some(Need::Permission))),
                Some(Status::Blocked(Some(Need::Question)))
            ),
            Some(Alert::NeedsYou)
        );
        assert_eq!(of(working, Some(Status::Error)), Some(Alert::Failed));
        // Repeats, cancelled turns and starting work say nothing new.
        assert_eq!(of(Some(Status::Done), Some(Status::Done)), None);
        assert_eq!(of(working, Some(Status::Idle)), None);
        assert_eq!(of(Some(Status::Idle), working), None);
        assert_eq!(of(working, None), None);
    }

    #[test]
    fn codex_finishes_by_going_idle() {
        let of = |old, new| Alert::of(Agent::Codex, old, new);
        assert_eq!(
            of(Some(Status::Working), Some(Status::Idle)),
            Some(Alert::Finished)
        );
        assert_eq!(of(None, Some(Status::Idle)), None);
    }

    #[test]
    fn chimes_stay_quiet_enough() {
        for kind in [Alert::Finished, Alert::NeedsYou, Alert::Failed] {
            let samples = chime(kind);
            assert!(!samples.is_empty());
            assert!(samples.iter().all(|s| s.abs() <= 0.3 + f32::EPSILON));
        }
    }

    #[test]
    fn escapes_markup_in_bodies() {
        assert_eq!(escape("Fix <Button> · R&D"), "Fix &lt;Button&gt; · R&amp;D");
        assert_eq!(escape("plain"), "plain");
    }
}
