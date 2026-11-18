//! Tells the user when an agent finishes or needs them while they're looking
//! elsewhere: with a desktop notification, a sound, or both, as the settings
//! ask.
//!
//! Notifications go to the desktop's notification server, which shows them
//! the way the user set it up. The sound is played here, so it plays without
//! a notification too, and the same on desktops whose server plays none.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::*;

use agentshub_platform::notifications::{self as dbus, Bus, DEFAULT_ACTION, Event, Note, Signal};

use crate::agent::Agent;
use crate::settings::Settings;
use crate::status::Status;
use crate::store::Item;
use crate::transcript::Kind;
use crate::workspace::{self, Workspace};

mod sound;

/// Sounds closer together than this play once, so agents finishing together
/// don't make a racket.
const SOUND_GAP: Duration = Duration::from_millis(1500);

/// How much of a reply or a tool call a notification quotes.
const DETAIL_CHARS: usize = 160;

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
    pub fn of(old: Option<Status>, new: Option<Status>) -> Option<Self> {
        if old == new {
            return None;
        }
        match new? {
            Status::Blocked(_) => Some(Self::NeedsYou),
            Status::Done => Some(Self::Finished),
            Status::Error => Some(Self::Failed),
            // Going back to idle is a cancelled turn, which the user did.
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
    /// The alert each chat has waiting on what it will say, by its hold.
    held: HashMap<i64, u64>,
    /// The last hold given.
    hold: u64,
}

impl Tracker {
    /// Whether there's anything about `chat` to take back.
    fn tracks(&self, chat: i64) -> bool {
        self.chats.contains_key(&chat) || self.held.contains_key(&chat)
    }

    /// Holds a place for an alert about `chat` that waits on what it will
    /// say. Holding another, or taking back the chat's notification, gives
    /// it up.
    fn hold(&mut self, chat: i64) -> u64 {
        self.hold += 1;
        self.held.insert(chat, self.hold);
        self.hold
    }

    /// Whether the alert with `hold` is still wanted, which it's not once
    /// it has been asked for.
    fn release(&mut self, chat: i64, hold: u64) -> bool {
        let held = self.held.get(&chat) == Some(&hold);
        if held {
            self.held.remove(&chat);
        }
        held
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
        self.held.remove(&chat);
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
    last_sound: Option<Instant>,
}

impl Global for Alerts {}

pub fn init(workspace: &Entity<Workspace>, cx: &mut App) {
    cx.set_global(Alerts {
        workspace: workspace.downgrade(),
        bus: None,
        tracker: Tracker::default(),
        last_sound: None,
    });

    let (events, received) = smol::channel::unbounded();
    cx.spawn(async move |cx| {
        let (bus, owner) = match cx.background_spawn(dbus::listen(events)).await {
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

/// Holds a place for an alert about `chat` that waits on what it will say,
/// so that it isn't sent once the user has seen the chat or a newer alert
/// about it has taken its place.
pub fn hold(chat: i64, cx: &mut App) -> u64 {
    cx.global_mut::<Alerts>().tracker.hold(chat)
}

/// Lets the user know `chat`'s agent did what `status` says, which made the
/// `kind` of alert given, after a turn that `took` this long if it ended.
/// `body` says which chat it is. Nothing is sent unless `hold` still is.
#[expect(
    clippy::too_many_arguments,
    reason = "GPUI's context arguments count toward the limit"
)]
pub fn alert(
    chat: i64,
    hold: u64,
    agent: Agent,
    kind: Alert,
    status: Status,
    took: Option<Duration>,
    body: String,
    cx: &mut App,
) {
    if !cx.global_mut::<Alerts>().tracker.release(chat, hold) {
        return;
    }
    let settings = *cx.global::<Settings>();
    if settings.notification_sound {
        sound::play(kind, cx);
    }
    if settings.notification_popups {
        let mut summary = crate::sidebar::activity(agent, Some(Some(status)));
        if let Some(took) = took {
            summary = format!("{summary} · {}", workspace::duration(took));
        }
        let urgent = kind != Alert::Finished;
        show(
            chat,
            Note {
                summary,
                body,
                urgent,
            },
            cx,
        );
    }
}

/// What the agent's last turn, at the end of `items`, says about the `kind`
/// of alert it made: the tool call it needs the user to allow or answer,
/// or else the last thing it said.
pub fn details(kind: Alert, items: &[Item]) -> Option<String> {
    let start = items
        .iter()
        .rposition(|item| item.kind == Kind::User)
        .map_or(0, |at| at + 1);
    let turn = &items[start..];
    let waiting = turn
        .iter()
        .rev()
        .filter(|_| kind == Alert::NeedsYou)
        .find(|item| item.kind == Kind::Tool && item.payload["output"].is_null())
        .map(|item| call(&item.payload));
    let said = || {
        turn.iter()
            .rev()
            .find(|item| item.kind == Kind::Assistant)
            .and_then(|item| item.payload["text"].as_str())
            .map(str::to_owned)
    };
    let text = clip(&waiting.or_else(said)?);
    (!text.is_empty()).then_some(text)
}

/// A tool call as its name and what it acts on, or the question it asks.
fn call(payload: &serde_json::Value) -> String {
    use serde_json::Value;

    let name = payload["name"].as_str().unwrap_or("tool");
    let input = &payload["input"];
    if let Some(question) = input["questions"][0]["question"].as_str() {
        return question.to_owned();
    }
    let target = [
        "command",
        "cmd",
        "file_path",
        "path",
        "url",
        "pattern",
        "query",
    ]
    .into_iter()
    .find_map(|key| match &input[key] {
        Value::String(text) => Some(text.clone()),
        // Codex gives commands as their arguments.
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    })
    .unwrap_or_else(|| match input {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        input => input.to_string(),
    });
    if target.trim().is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {target}")
    }
}

/// `text` on one line, cut short at `DETAIL_CHARS`.
#[expect(clippy::string_slice, reason = "`at` comes from `char_indices`")]
fn clip(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(DETAIL_CHARS) {
        Some((at, _)) => format!("{}…", line[..at].trim_end()),
        None => line,
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
        sound::play(Alert::Finished, cx);
    }
    if settings.notification_popups {
        let Some(bus) = cx.global::<Alerts>().bus.clone() else {
            eprintln!("can't show a notification: no notification server");
            return;
        };
        let note = Note {
            summary: "Notifications are on".into(),
            body: "Agents Hub tells you like this when an agent finishes or needs you.".into(),
            urgent: false,
        };
        cx.background_spawn(async move {
            if let Err(err) = dbus::test(&bus, note).await {
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
/// it's 0.
fn send(ticket: u64, note: Note, replaces: u32, cx: &mut App) {
    // An id means something only to the server that gave it, so a
    // replacement goes to that server alone. A new one goes to whichever
    // server there is, which the bus starts if need be.
    let Some(bus) = cx.global::<Alerts>().bus.clone() else {
        failed(ticket, "no notification server", cx);
        return;
    };
    let destination = if replaces == 0 {
        None
    } else {
        let Some(owner) = cx.global::<Alerts>().tracker.owner.clone() else {
            failed(ticket, "no notification server", cx);
            return;
        };
        Some(owner)
    };
    cx.spawn(async move |cx| {
        let sent = cx
            .background_spawn(async move {
                dbus::send(&bus, destination.as_deref(), note, replaces).await
            })
            .await;
        cx.update(|cx| {
            let effect = cx.global_mut::<Alerts>().tracker.replied(ticket, sent);
            run(effect, cx);
        });
    })
    .detach();
}

/// Tells the tracker that the notification with `ticket` couldn't be sent.
fn failed(ticket: u64, error: &str, cx: &mut App) {
    let effect = cx
        .global_mut::<Alerts>()
        .tracker
        .replied(ticket, Err(error.into()));
    run(effect, cx);
}

/// Takes notification `id` off the screen of the server that showed it.
fn close(id: u32, cx: &mut App) {
    let alerts = cx.global::<Alerts>();
    let (Some(bus), Some(owner)) = (alerts.bus.clone(), alerts.tracker.owner.clone()) else {
        return;
    };
    cx.background_spawn(async move {
        dbus::close(&bus, &owner, id).await;
    })
    .detach();
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

#[cfg(test)]
mod tests {
    // Not a glob: GPUI's `test` attribute would take the place of the
    // standard one.
    use serde_json::json;

    use super::sound::chime;
    use super::{Alert, Click, Effect, Note, Signal, Tracker, details};
    use crate::status::{Need, Status};
    use crate::store::Item;
    use crate::transcript::Kind;

    fn note(text: &str) -> Note {
        Note {
            summary: text.into(),
            body: text.into(),
            urgent: false,
        }
    }

    fn item(kind: Kind, payload: serde_json::Value) -> Item {
        Item {
            id: 0,
            kind,
            payload,
        }
    }

    fn text(kind: Kind, text: &str) -> Item {
        item(kind, json!({ "text": text }))
    }

    fn tool(name: &str, input: serde_json::Value, output: Option<&str>) -> Item {
        item(
            Kind::Tool,
            json!({ "id": "1", "name": name, "input": input, "output": output }),
        )
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
    fn held_alerts_go_once_seen_or_replaced() {
        let mut tracker = tracker();
        let first = tracker.hold(7);
        // A newer alert takes its place.
        let second = tracker.hold(7);
        assert!(!tracker.release(7, first));
        assert!(tracker.release(7, second));
        assert!(!tracker.release(7, second));
        assert!(!tracker.tracks(7));

        // The user saw the chat while it waited.
        let held = tracker.hold(7);
        assert!(tracker.tracks(7));
        assert_eq!(tracker.withdraw(7), None);
        assert!(!tracker.release(7, held));

        // Other chats' alerts and a change of server don't touch it.
        let held = tracker.hold(7);
        tracker.hold(8);
        tracker.withdraw(8);
        tracker.owner_changed(Some(":1.2".into()));
        assert!(tracker.release(7, held));
    }

    #[test]
    fn alerts_on_what_needs_the_user() {
        let working = Some(Status::Working);
        let of = Alert::of;
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
    fn chimes_stay_quiet_enough() {
        for kind in [Alert::Finished, Alert::NeedsYou, Alert::Failed] {
            let samples = chime(kind);
            assert!(!samples.is_empty());
            assert!(samples.iter().all(|s| s.abs() <= 0.3 + f32::EPSILON));
        }
    }

    #[test]
    fn details_quote_the_last_reply_of_the_turn() {
        let items = [
            text(Kind::User, "first"),
            text(Kind::Assistant, "old reply"),
            text(Kind::User, "second"),
            text(Kind::Assistant, "Looking."),
            tool("Read", json!({ "file_path": "a.rs" }), Some("…")),
            text(Kind::Assistant, "Fixed it.\n\n  Tests   pass."),
        ];
        assert_eq!(
            details(Alert::Finished, &items).as_deref(),
            Some("Fixed it. Tests pass.")
        );
        // Nothing said since the prompt.
        assert_eq!(details(Alert::Finished, &items[..3]), None);
        assert_eq!(details(Alert::Finished, &[]), None);
    }

    #[test]
    fn details_say_what_the_agent_waits_on() {
        let mut items = vec![
            text(Kind::User, "clean up"),
            text(Kind::Assistant, "Removing the build."),
            tool("Bash", json!({ "command": "rm -rf target" }), None),
        ];
        assert_eq!(
            details(Alert::NeedsYou, &items).as_deref(),
            Some("Bash: rm -rf target")
        );
        // Only a call that hasn't run is waited on.
        assert_eq!(
            details(Alert::Finished, &items).as_deref(),
            Some("Removing the build.")
        );
        items.push(tool(
            "AskUserQuestion",
            json!({ "questions": [{ "question": "Keep the cache?" }] }),
            None,
        ));
        assert_eq!(
            details(Alert::NeedsYou, &items).as_deref(),
            Some("Keep the cache?")
        );
        items.push(tool(
            "shell",
            json!({ "command": ["bash", "-lc", "ls"] }),
            None,
        ));
        assert_eq!(
            details(Alert::NeedsYou, &items).as_deref(),
            Some("shell: bash -lc ls")
        );
    }

    #[test]
    fn details_are_cut_short() {
        let long = "word ".repeat(100);
        let clipped = details(Alert::Finished, &[text(Kind::Assistant, &long)]).unwrap();
        assert_eq!(clipped.chars().count(), 160);
        assert!(clipped.ends_with("word…"));
    }
}
