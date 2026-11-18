//! Small formatting helpers shared by the app's views.

use crate::store;

/// How long ago something happened, given in seconds since the Unix epoch,
/// in its largest whole unit.
pub fn age(at: i64) -> String {
    let minutes = (store::now() - at).max(0) / 60;
    match minutes {
        0 => "now".to_string(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        1440..10080 => format!("{}d", minutes / 1440),
        10080..525600 => format!("{}w", minutes / 10080),
        _ => format!("{}y", minutes / 525600),
    }
}

use std::time::{Duration, Instant};

/// How long an agent has been on the user's last prompt, waiting on the user
/// included, or how long it took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Turn {
    Since(Instant),
    Took(Duration),
}

impl Turn {
    /// The turn after an agent's status changes, from busy (working or
    /// waiting on the user) or not to busy or not.
    pub fn after(turn: Option<Self>, was_busy: bool, busy: bool, now: Instant) -> Option<Self> {
        match (was_busy, busy, turn) {
            (false, true, _) => Some(Self::Since(now)),
            (true, false, Some(Self::Since(start))) => Some(Self::Took(now - start)),
            (_, _, turn) => turn,
        }
    }
}

/// A duration in its two largest units, padded so it keeps its width while
/// it counts up.
pub fn duration(time: Duration) -> String {
    let seconds = time.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// "1 chat", "2 chats".
pub fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}
