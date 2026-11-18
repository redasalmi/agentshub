//! Tests for the workspace's shared helpers and shutdown import.

use super::{
    Lifecycle, MAX_SHELL_SHARE, MIN_SHELL_HEIGHT, Turn, duration, import_chat, reported_sessions,
    shell_height, shell_share, take_and_remove,
};
use crate::store::{Db, Store};
use crate::worktree::{Removal, Worktree};
use crate::{agent::Agent, reports::Reports, transcript};
use gpui_kit::px;
use serde_json::json;
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

fn message(id: &str, parent: Option<&str>, role: &str, text: &str) -> String {
    format!(
        "{}\n",
        json!({
            "type": role, "uuid": id, "parentUuid": parent,
            "message": { "role": role, "content": [{ "type": "text", "text": text }] }
        })
    )
}

#[test]
fn shutdown_imports_unread_turns_and_sessions_in_queued_reports() {
    let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("agentshub.db");
    let db = Db::open(&path).unwrap();
    let project = db.add_project(&dir).unwrap();
    let chat = db.add_chat(project, Agent::Claude, None).unwrap();
    let first = dir.join("first.jsonl");
    let second = dir.join("second.jsonl");
    let prompt = message("1", None, "user", "first prompt");
    fs::write(&first, &prompt).unwrap();
    let session = db
        .record_session(chat, Agent::Claude, "first", &first, Some("startup"))
        .unwrap()
        .session_id;
    transcript::read(&db, session).unwrap();
    // No status report triggers a read of the last answer.
    fs::write(
        &first,
        format!(
            "{prompt}{}",
            message("2", Some("1"), "assistant", "last answer")
        ),
    )
    .unwrap();
    fs::write(&second, message("3", None, "user", "second prompt")).unwrap();
    let (mut reports, _) = Reports::new();
    // This session switch is still queued when the workspace is released.
    fs::write(
        reports.path("cli"),
        format!(
            "{}\n",
            json!({
                "session_id": "second", "transcript_path": second, "source": "clear"
            })
        ),
    )
    .unwrap();

    let store = Store::new(db.clone()).unwrap();
    import_chat(&store, &mut reports, chat, Agent::Claude, "cli", Vec::new()).unwrap();
    assert!(reports.read("cli", Agent::Claude).is_empty());
    reports.remove();
    // The original transcripts can expire; the database still has both.
    fs::remove_file(&first).unwrap();
    fs::remove_file(&second).unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    let sessions = db.sessions(chat).unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|session| session.prompted));
    assert_eq!(
        db.items(sessions[0].id, 100).unwrap()[1].payload["text"],
        "last answer"
    );
    assert_eq!(
        db.items(sessions[1].id, 100).unwrap()[0].payload["text"],
        "second prompt"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// Restarting a chat kills its CLI and opens the chat again on its last
/// recorded session. A report the CLI left queued says it had gone into
/// another one, so it has to be recorded before the CLI goes, or the
/// restart resumes a session the CLI had already left and the one it was
/// really in is never recorded.
#[test]
fn restarting_records_the_session_the_cli_reported() {
    let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let db = Db::open_in_memory().unwrap();
    let project = db.add_project(&dir).unwrap();
    let chat = db.add_chat(project, Agent::Claude, None).unwrap();
    let first = dir.join("first.jsonl");
    let second = dir.join("second.jsonl");
    fs::write(&first, message("1", None, "user", "first prompt")).unwrap();
    fs::write(&second, message("2", None, "user", "second prompt")).unwrap();
    db.record_session(chat, Agent::Claude, "first", &first, Some("startup"))
        .unwrap();
    let (mut reports, _) = Reports::new();
    // The CLI reported the switch, and the report is still queued.
    fs::write(
        reports.path("cli"),
        format!(
            "{}\n",
            json!({
                "session_id": "second", "transcript_path": second, "source": "clear"
            })
        ),
    )
    .unwrap();
    let last = |db: &Db| db.sessions(chat).unwrap().pop().unwrap().agent_id;
    assert_eq!(last(&db), "first");

    let queued = reported_sessions(&mut reports, "cli", Agent::Claude);
    assert_eq!(
        queued,
        vec![(
            "second".to_string(),
            second.clone(),
            Some("clear".to_string())
        )]
    );
    for (agent_id, path, reason) in queued {
        db.record_session(chat, Agent::Claude, &agent_id, &path, reason.as_deref())
            .unwrap();
    }
    // What opening the chat again resumes.
    assert_eq!(last(&db), "second");
    // The reports are taken out, so stopping leaves nothing behind.
    assert!(reported_sessions(&mut reports, "cli", Agent::Claude).is_empty());

    reports.remove();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shutdown_continues_importing_after_one_transcript_fails() {
    let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let db = Db::open_in_memory().unwrap();
    let project = db.add_project(&dir).unwrap();
    let chat = db.add_chat(project, Agent::Claude, None).unwrap();
    // A directory cannot be read as a transcript.
    let bad = db
        .record_session(chat, Agent::Claude, "bad", &dir, None)
        .unwrap()
        .session_id;
    let good = dir.join("good.jsonl");
    fs::write(&good, message("1", None, "user", "kept")).unwrap();
    let session = db
        .record_session(chat, Agent::Claude, "good", &good, None)
        .unwrap()
        .session_id;
    let (mut reports, _) = Reports::new();

    let sessions = vec![bad, session];
    let store = Store::new(db.clone()).unwrap();
    assert!(import_chat(&store, &mut reports, chat, Agent::Claude, "cli", sessions).is_err());
    assert_eq!(db.items(session, 100).unwrap()[0].payload["text"], "kept");
    reports.remove();
    fs::remove_dir_all(dir).unwrap();
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
