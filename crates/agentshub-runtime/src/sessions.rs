//! Following the sessions the chats' CLIs go into: the ones they report, and
//! for Codex, which doesn't report them, the ones its session files show.

use std::path::PathBuf;

use anyhow::Result;

use agentshub_core::agent::Agent;
use agentshub_core::reports::Reports;
use agentshub_core::store::Store;

/// The sessions in a terminal's queued reports, taken out of them, in the
/// order its CLI went into them.
pub fn reported_sessions(
    reports: &mut Reports,
    key: &str,
    agent: Agent,
) -> Vec<(String, PathBuf, Option<String>)> {
    reports
        .read(key, agent)
        .into_iter()
        .map(|(agent_id, report)| (agent_id, report.transcript_path, report.reason))
        .collect()
}

/// Records the sessions a chat's CLI reported last, and reads `sessions`
/// along with the ones those reports moved the chat into or out of. One
/// transcript that can't be read doesn't stop the others.
pub fn import_chat(
    store: &Store,
    reports: &mut Reports,
    id: i64,
    agent: Agent,
    key: &str,
    mut sessions: Vec<i64>,
) -> Result<()> {
    let mut failure = None;
    for (agent_id, report) in reports.read(key, agent) {
        let recorded =
            store.record_session(id, agent, agent_id, report.transcript_path, report.reason);
        match smol::block_on(recorded) {
            Ok(recorded) => {
                sessions.push(recorded.session_id);
                if let Some((_, read)) = recorded.left
                    && let Err(err) =
                        smol::block_on(read.recv()).unwrap_or_else(|err| Err(err.into()))
                {
                    failure.get_or_insert(err);
                }
            }
            Err(err) => {
                failure.get_or_insert(err);
            }
        }
    }
    sessions.sort_unstable();
    sessions.dedup();
    for session in sessions {
        if let Err(err) = smol::block_on(store.read_transcript(session)) {
            failure.get_or_insert(err);
        }
    }
    match failure {
        Some(err) => Err(err),
        None => Ok(()),
    }
}
