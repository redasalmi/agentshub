//! The sidebar's text: project marks, paths, ages and status summaries.

use super::*;

pub fn subtitle(chat: &Chat, tone: Tone, project: Option<&str>) -> String {
    let source = project.unwrap_or(chat.agent.label());
    match tone {
        Tone::Saved => format!("{source} · {}", age(chat.updated_at)),
        tone => format!("{} · {source} · {}", tone.label(), age(chat.updated_at)),
    }
}

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

/// `path` on one line, cut from its start when it doesn't fit, since its end
/// says the most, and whole in a tooltip.
pub fn path_line(path: &Path, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let shown = home_relative(path);
    let whole = SharedString::from(path.display().to_string());
    div()
        .id(SharedString::from(format!("path:{whole}")))
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis_start()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.muted_foreground)
        .tooltip(move |window, cx| Tooltip::new(whole.clone()).build(window, cx))
        .child(shown)
}

/// `path`, starting with `~` when it is in the home folder.
pub fn home_relative(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

pub fn activity(agent: Agent, running: Option<Option<Status>>) -> String {
    let name = agent.display_name();
    match running {
        None => name.to_string(),
        Some(None) => format!("{name} is running"),
        Some(Some(Status::Idle)) => format!("{name} is idle"),
        Some(Some(Status::Working)) => format!("{name} is working"),
        Some(Some(Status::Blocked(Some(Need::Permission)))) => {
            format!("{name} needs your permission")
        }
        Some(Some(Status::Blocked(Some(Need::Question)))) => format!("{name} has a question"),
        Some(Some(Status::Blocked(Some(Need::Auth)))) => format!("{name} needs you to sign in"),
        Some(Some(Status::Blocked(None))) => format!("{name} is waiting for you"),
        Some(Some(Status::Done)) => format!("{name} is done"),
        Some(Some(Status::Error)) => format!("{name} stopped on an error"),
    }
}

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

/// How many agents need the user, work, sit idle, or haven't said.
pub fn summary<'a>(running: impl Iterator<Item = &'a Option<Status>>) -> String {
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
