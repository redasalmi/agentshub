//! Which group each chat belongs to, and the order Triage lists them in.

use agentshub_ui_kit::status::Tone;

/// A section of the triage list, in the order the list shows them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Group {
    NeedsYou,
    ReadyToReview,
    Working,
    Idle,
    Closed,
    Archived,
}

/// A chat's place in the triage list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub chat: i64,
    pub group: Group,
    pub pinned: bool,
}

/// The selected chat and the group it was in when it was selected, so that
/// it doesn't jump away while the user reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Kept {
    pub chat: i64,
    pub group: Group,
}

pub fn group_of(tone: Tone, archived: bool) -> Group {
    if archived {
        return Group::Archived;
    }
    match tone {
        Tone::NeedsYou(_) | Tone::Error => Group::NeedsYou,
        Tone::Done => Group::ReadyToReview,
        Tone::Running | Tone::Working => Group::Working,
        Tone::Idle => Group::Idle,
        Tone::Saved => Group::Closed,
    }
}

/// The rows of `chats` (id, tone, pinned, archived, project) in list order:
/// by group, pinned first, then as `chats` comes. `filter` keeps one
/// project's chats.
pub fn rows(
    chats: &[(i64, Tone, bool, bool, i64)],
    filter: Option<i64>,
    kept: Option<Kept>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = chats
        .iter()
        .filter(|&&(.., project)| filter.is_none_or(|wanted| wanted == project))
        .map(|&(chat, tone, pinned, archived, _)| Row {
            chat,
            group: match kept {
                Some(kept) if kept.chat == chat => kept.group,
                _ => group_of(tone, archived),
            },
            pinned,
        })
        .collect();
    rows.sort_by_key(|row| (row.group, !row.pinned));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: i64, tone: Tone) -> (i64, Tone, bool, bool, i64) {
        (id, tone, false, false, 1)
    }

    fn ids(rows: &[Row]) -> Vec<i64> {
        rows.iter().map(|row| row.chat).collect()
    }

    #[test]
    fn needs_and_errors_lead() {
        let chats = [
            chat(1, Tone::Idle),
            chat(2, Tone::NeedsYou(None)),
            chat(3, Tone::Error),
        ];
        let rows = rows(&chats, None, None);
        assert_eq!(ids(&rows), [2, 3, 1]);
        assert_eq!(rows[0].group, Group::NeedsYou);
        assert_eq!(rows[1].group, Group::NeedsYou);
    }

    #[test]
    fn done_is_ready_to_review() {
        assert_eq!(group_of(Tone::Done, false), Group::ReadyToReview);
    }

    #[test]
    fn running_and_working_are_working() {
        assert_eq!(group_of(Tone::Running, false), Group::Working);
        assert_eq!(group_of(Tone::Working, false), Group::Working);
    }

    #[test]
    fn saved_is_closed_and_archived_is_last() {
        assert_eq!(group_of(Tone::Saved, false), Group::Closed);
        assert_eq!(group_of(Tone::Saved, true), Group::Archived);
        let chats = [
            (1, Tone::Saved, false, true, 1),
            (2, Tone::Saved, false, false, 1),
        ];
        assert_eq!(ids(&rows(&chats, None, None)), [2, 1]);
    }

    #[test]
    fn pinned_chats_lead_their_group() {
        let chats = [
            (1, Tone::Idle, false, false, 1),
            (2, Tone::Idle, true, false, 1),
        ];
        assert_eq!(ids(&rows(&chats, None, None)), [2, 1]);
    }

    #[test]
    fn the_filter_keeps_one_project() {
        let chats = [
            (1, Tone::Idle, false, false, 1),
            (2, Tone::Idle, false, false, 2),
        ];
        assert_eq!(ids(&rows(&chats, Some(2), None)), [2]);
    }

    #[test]
    fn a_done_chat_keeps_its_group_while_selected() {
        let chats = [chat(7, Tone::Idle)];
        let kept = Kept {
            chat: 7,
            group: Group::ReadyToReview,
        };
        assert_eq!(
            rows(&chats, None, Some(kept))[0].group,
            Group::ReadyToReview
        );
    }
}
