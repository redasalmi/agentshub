//! Tests for the workspace's shared helpers and shutdown import.

use super::{Turn, duration, take_and_remove};
use crate::chat::{MAX_SHELL_SHARE, MIN_SHELL_HEIGHT, shell_height, shell_share};
use agentshub_core::lifecycle::Lifecycle;
use agentshub_core::worktree::{Removal, Worktree};
use gpui_kit::px;
use std::fs;
use std::time::{Duration, Instant};

#[test]
fn times_turns_from_busy_to_at_rest() {
    let start = Instant::now();
    let later = start + Duration::from_secs(90);
    let since = Some(Turn::Since(start));
    let took = Some(Turn::Took(Duration::from_secs(5)));
    assert_eq!(Turn::after(None, false, true, start), since);
    // A new prompt starts over.
    assert_eq!(Turn::after(took, false, true, start), since);
    // Waiting on the user, or back at work, is still the same turn.
    assert_eq!(Turn::after(since, true, true, later), since);
    assert_eq!(
        Turn::after(since, true, false, later),
        Some(Turn::Took(Duration::from_secs(90)))
    );
    // At rest, the last turn stays.
    assert_eq!(Turn::after(took, false, false, later), took);
    assert_eq!(Turn::after(None, false, false, later), None);
}

#[test]
fn gives_durations_in_two_units() {
    let of = |seconds| duration(Duration::from_secs(seconds));
    assert_eq!(of(0), "0s");
    assert_eq!(of(59), "59s");
    assert_eq!(of(60), "1m 00s");
    assert_eq!(of(192), "3m 12s");
    assert_eq!(of(3599), "59m 59s");
    assert_eq!(of(3840), "1h 04m");
}

#[test]
fn keeps_the_shell_within_its_limits() {
    // As dragged, within its limits.
    assert_eq!(shell_share(px(300.), px(1000.)), 0.3);
    assert_eq!(shell_share(px(10.), px(1000.)), 0.096);
    assert_eq!(shell_share(px(990.), px(1000.)), MAX_SHELL_SHARE);
    // Drawn the share of the window it takes, never shorter than its
    // header and a few lines.
    assert_eq!(shell_height(0.3, px(1000.)), px(300.));
    assert_eq!(shell_height(0.3, px(200.)), MIN_SHELL_HEIGHT);
    assert_eq!(shell_height(0.99, px(1000.)), px(750.));
}

#[test]
fn a_pending_worktree_removal_is_done_once() {
    let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let root = dir.join("gone");
    let worktree = Worktree {
        root: root.clone(),
        folder: root,
    };
    let mut lifecycle = Lifecycle::default();
    let job = lifecycle.track_removal(&worktree, &dir, Vec::new());
    // Whichever of its task and shutdown comes first removes it.
    assert_eq!(take_and_remove(&job).unwrap().unwrap(), Removal::Removed);
    assert!(take_and_remove(&job).is_none());
    assert!(
        lifecycle
            .take_removals()
            .iter()
            .all(|job| take_and_remove(job).is_none())
    );
    fs::remove_dir_all(dir).ok();
}
