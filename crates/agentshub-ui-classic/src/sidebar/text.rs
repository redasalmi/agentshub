//! The sidebar's text: paths, ages and status summaries.

use super::*;

/// How many agents need the user, work, sit idle, or haven't said.
pub(super) fn summary<'a>(running: impl Iterator<Item = &'a Option<Status>>) -> String {
    let (mut working, mut need_you, mut idle, mut unknown) = (0, 0, 0, 0);
    for status in running {
        match status {
            Some(Status::Working) => working += 1,
            Some(Status::Blocked(_) | Status::Done | Status::Error) => need_you += 1,
            Some(Status::Idle) => idle += 1,
            None => unknown += 1,
        }
    }
    let mut parts = Vec::new();
    match need_you {
        0 => {}
        1 => parts.push("1 needs you".to_string()),
        n => parts.push(format!("{n} need you")),
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if idle > 0 {
        parts.push(format!("{idle} idle"));
    }
    if unknown > 0 {
        parts.push(format!("{unknown} running"));
    }
    if parts.is_empty() {
        "No agents running".to_string()
    } else {
        parts.join(" · ")
    }
}
