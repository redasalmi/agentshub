use std::fs;

use agentshub_app::catalog::Catalog;
use agentshub_core::agent::Agent;
use agentshub_core::reports::Reports;
use agentshub_core::store::{Change, Db, Store};
use agentshub_core::transcript;
use serde_json::json;

use crate::runtime::apply_change;
use crate::sessions::{import_chat, reported_sessions};

#[test]
fn a_change_reaches_the_catalog_before_it_is_saved() {
    let db = Db::open_in_memory().unwrap();
    let project = db
        .add_project(&std::env::temp_dir().join("agentshub-project"))
        .unwrap();
    let first = db.add_chat(project, Agent::Claude, None).unwrap();
    let second = db.add_chat(project, Agent::Codex, None).unwrap();
    let store = Store::new(db).unwrap();
    let mut catalog = Catalog::new(smol::block_on(store.projects()).unwrap());
    // A chat is added on top.
    assert_eq!(catalog.projects()[0].chats[0].id, second);

    let saved = apply_change(
        &mut catalog,
        &store,
        Change::Pin {
            chat: first,
            pinned: true,
        },
    );
    assert_eq!(catalog.projects()[0].chats[0].id, first);
    assert_eq!(catalog.unsaved(), 1);

    smol::block_on(saved).unwrap();
    let listed = smol::block_on(store.projects()).unwrap();
    assert_eq!(listed[0].chats[0].id, first);
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
        vec![("second".to_string(), second, Some("clear".to_string()))]
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
