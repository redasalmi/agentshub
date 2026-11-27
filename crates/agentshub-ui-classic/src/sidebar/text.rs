//! The sidebar's text: project marks, paths, ages and status summaries.

use super::*;

pub fn marks<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut taken = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let mark = candidates(name)
                .into_iter()
                .find(|mark| !taken.contains(mark))
                .unwrap_or_else(|| initials(name));
            taken.insert(mark.clone());
            mark
        })
        .collect()
}

/// The marks project `name` could have, the likeliest first.
fn candidates(name: &str) -> Vec<String> {
    let letters: Vec<char> = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let mut candidates = vec![initials(name)];
    if let [first, second, rest @ ..] = letters.as_slice() {
        let pairs = std::iter::once(second).chain(rest);
        candidates.extend(pairs.map(|other| [*first, *other].iter().collect::<String>()));
        candidates.extend(
            rest.iter()
                .map(|other| [*first, *second, *other].iter().collect()),
        );
    }
    candidates
}

/// The two letters a project's mark shows unless another project has them:
/// the first of its first two words, or its first two letters.
pub(super) fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [word] => word.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_lowercase()
}

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
