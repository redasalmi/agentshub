//! agentshub's own projects and chats, the agent sessions each chat went
//! through, and a trimmed copy of those sessions (see `transcript`), in SQLite.
//!
//! Writes go through one connection, which the transcript reads on the
//! background executor hold for the length of their transaction. Reads have a
//! connection of their own, so that the UI doesn't wait on those writes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use anyhow::{Context as _, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, Result, params};
use serde_json::{Value, json};

use crate::agent::Agent;
use crate::transcript::{self, Kind};

/// Each migration takes the database from the version before it, as counted
/// by `PRAGMA user_version`, to the next.
const MIGRATIONS: [&str; 4] = [
    r#"
CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    path       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE chats (
    id         INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects ON DELETE CASCADE,
    agent      TEXT NOT NULL,
    title      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX chats_project ON chats (project_id);

-- One row per agent session, shared by the chats it appears in. Its title is
-- the custom one, then the generated one, then its first prompt.
CREATE TABLE sessions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    agent           TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    transcript_path TEXT NOT NULL,
    custom_title    TEXT,
    generated_title TEXT,
    prompt          TEXT,
    started_at      INTEGER NOT NULL,
    read_offset     INTEGER NOT NULL DEFAULT 0,
    missing         INTEGER NOT NULL DEFAULT 0,
    UNIQUE (agent, agent_id)
);

-- The sessions of a chat, in order. The last one is resumed.
CREATE TABLE chat_sessions (
    chat_id    INTEGER NOT NULL REFERENCES chats ON DELETE CASCADE,
    session_id INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    reason     TEXT,
    PRIMARY KEY (chat_id, session_id),
    UNIQUE (chat_id, position)
);
CREATE INDEX chat_sessions_session ON chat_sessions (session_id);

-- The trimmed copy of each session.
CREATE TABLE items (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    source_id  TEXT,
    parent_id  TEXT,
    kind       TEXT NOT NULL,
    at         INTEGER,
    payload    TEXT NOT NULL,
    UNIQUE (session_id, source_id)
);
CREATE INDEX items_session ON items (session_id, id);

-- Transcript lines that aren't kept but sit in a branch between kept ones,
-- so that a branch can be followed back through them.
CREATE TABLE links (
    session_id INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    source_id  TEXT NOT NULL,
    parent_id  TEXT,
    PRIMARY KEY (session_id, source_id)
) WITHOUT ROWID;
"#,
    r#"
-- Why the chat left a session. Its own reason says why it went into it.
ALTER TABLE chat_sessions ADD COLUMN ended_reason TEXT;

-- Tool outputs are matched to their call by the call's id in the payload, so
-- each one only looks at the session's other tool calls.
CREATE INDEX items_tool_call ON items (session_id, json_extract(payload, '$.id'))
    WHERE kind = 'tool';
"#,
    r#"
-- Whether a session has a prompt, which an image-only prompt has without a
-- title. `prompt` stays the first prompt text, for the title.
ALTER TABLE sessions ADD COLUMN prompted INTEGER NOT NULL DEFAULT 0;
UPDATE sessions SET prompted = (prompt IS NOT NULL OR EXISTS (
    SELECT 1 FROM items WHERE items.session_id = sessions.id AND items.kind = 'user'
));

-- The last entry of the transcript, which can be one that isn't displayed.
ALTER TABLE sessions ADD COLUMN leaf TEXT;

-- Where a session first entered a chat, which its position loses when the
-- chat resumes it again. It orders the sessions a chat is titled after.
ALTER TABLE chat_sessions ADD COLUMN first_position INTEGER;
UPDATE chat_sessions SET first_position = position;

-- Session ids must not be reused: a transcript read that started before its
-- chat was deleted must not find the session of a chat created afterwards.
CREATE TABLE sequences (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
INSERT INTO sequences (name, value)
VALUES ('session', COALESCE((SELECT MAX(id) FROM sessions), 0));
"#,
    r#"
-- Deferred chat opens must never find a replacement for a deleted chat.
INSERT INTO sequences (name, value)
VALUES ('chat', COALESCE((SELECT MAX(id) FROM chats), 0));
"#,
];

/// The schema version this build writes. The last step isn't SQL: it walks the
/// line-level ancestry the first four keep, resolves it into item ids, and
/// drops it, so a branch can be followed with a single indexed walk.
const SCHEMA: i64 = 5;

const TITLE: &str = "COALESCE(s.custom_title, s.generated_title, s.prompt)";

/// Where the database lives: `$XDG_DATA_HOME/agentshub/agentshub.db`.
pub fn path() -> Option<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::home_dir()?.join(".local/share"),
    };
    Some(data.join("agentshub").join("agentshub.db"))
}

/// Seconds since the Unix epoch, which is how the database keeps time.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

pub struct Project {
    pub id: i64,
    pub path: PathBuf,
    pub name: String,
    /// Newest first.
    pub chats: Vec<Chat>,
}

pub struct Chat {
    pub id: i64,
    pub project_id: i64,
    pub agent: Agent,
    pub title: Option<String>,
    pub updated_at: i64,
}

/// A session as one chat went through it.
pub struct Session {
    pub id: i64,
    pub agent_id: String,
    pub transcript_path: PathBuf,
    pub title: Option<String>,
    /// Whether it has a prompt, without which it isn't shown.
    pub prompted: bool,
    pub started_at: i64,
    pub missing: bool,
    /// Why the chat went into it: `startup`, `clear`, `new`, `resume`, `fork`.
    pub reason: Option<String>,
    /// Why the chat left it for another session.
    pub ended: Option<String>,
}

/// What a read of a session's transcript starts from.
pub struct Transcript {
    pub agent: Agent,
    pub agent_id: String,
    pub path: PathBuf,
    pub offset: u64,
}

/// What recording a reported session changed.
#[derive(Debug, PartialEq)]
pub struct Recorded {
    pub session_id: i64,
    /// The session the chat was in before, if it switched away from one.
    pub left: Option<i64>,
}

pub struct Item {
    pub id: i64,
    pub kind: Kind,
    pub payload: Value,
}

/// What saving a transcript read did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    /// The session was deleted while its transcript was being parsed.
    Gone,
    /// Another read saved first, from the same offset.
    Stale,
    /// Saved, but nothing the history shows changed.
    Unchanged,
    /// Saved, and the history changed.
    Changed,
}

impl Saved {
    /// Whether the read was saved and added anything the history shows.
    pub fn changed(self) -> bool {
        self == Saved::Changed
    }
}

#[derive(Clone)]
pub struct Db {
    write: Arc<Mutex<Connection>>,
    /// Reads get their own connection. In memory, there is only one.
    read: Option<Arc<Mutex<Connection>>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            create_dir(dir).with_context(|| format!("can't create {} first", dir.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("can't open {}", path.display()))?;
        // The transcripts in it are as private as the reports are.
        restrict(path);
        Self::init(conn, Some(path))
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(mut conn: Connection, path: Option<&Path>) -> anyhow::Result<Self> {
        set_pragmas(&conn)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA {
            bail!(
                "the database was written by a newer Agents Hub (schema {version}, this one knows {SCHEMA})"
            );
        }
        // A migration rewrites conversations that can't be recovered anywhere
        // else, so an existing database is copied before it changes.
        if version > 0
            && version < SCHEMA
            && let Some(path) = path
        {
            backup(&conn, path, version)?;
        }
        for (done, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(migration)?;
            tx.pragma_update(None, "user_version", done as i64 + 1)?;
            tx.commit()?;
        }
        if version < SCHEMA {
            let tx = conn.transaction()?;
            resolve_ancestry(&tx)?;
            tx.pragma_update(None, "user_version", SCHEMA)?;
            tx.commit()?;
        }
        let read = match path {
            Some(path) => {
                let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                // Reads only wait for a writer's commit, never for each other.
                conn.busy_timeout(BUSY_TIMEOUT)?;
                Some(Arc::new(Mutex::new(conn)))
            }
            None => None,
        };
        Ok(Self {
            write: Arc::new(Mutex::new(conn)),
            read,
        })
    }

    fn write(&self) -> MutexGuard<'_, Connection> {
        lock(&self.write)
    }

    fn read(&self) -> MutexGuard<'_, Connection> {
        lock(self.read.as_ref().unwrap_or(&self.write))
    }

    /// The projects, in the order they were added, newest first, with their
    /// chats.
    pub fn projects(&self) -> Result<Vec<Project>> {
        let conn = self.read();
        let mut chats: HashMap<i64, Vec<Chat>> = HashMap::new();
        // A chat is titled after the first of its sessions that has a title.
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.project_id, c.agent, c.updated_at, COALESCE(c.title, (
                SELECT {TITLE} FROM chat_sessions cs JOIN sessions s ON s.id = cs.session_id
                WHERE cs.chat_id = c.id AND {TITLE} IS NOT NULL
                ORDER BY COALESCE(cs.first_position, cs.position) LIMIT 1
            ))
            FROM chats c ORDER BY c.updated_at DESC, c.id DESC"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(Chat {
                id: row.get(0)?,
                project_id: row.get(1)?,
                agent: agent(&row.get::<_, String>(2)?)?,
                updated_at: row.get(3)?,
                title: row.get(4)?,
            })
        })?;
        for chat in rows {
            let chat = chat?;
            chats.entry(chat.project_id).or_default().push(chat);
        }

        let mut stmt =
            conn.prepare("SELECT id, path, name FROM projects ORDER BY created_at DESC, id DESC")?;
        stmt.query_map([], |row| {
            let id = row.get(0)?;
            Ok(Project {
                id,
                path: PathBuf::from(row.get::<_, String>(1)?),
                name: row.get(2)?,
                chats: chats.remove(&id).unwrap_or_default(),
            })
        })?
        .collect()
    }

    /// Adds a folder as a project, or finds the project it already is.
    pub fn add_project(&self, path: &Path) -> Result<i64> {
        let conn = self.write();
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let path = path.to_string_lossy();
        conn.execute(
            "INSERT INTO projects (path, name, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (path) DO NOTHING",
            params![path, name, now()],
        )?;
        conn.query_row("SELECT id FROM projects WHERE path = ?1", [path], |row| {
            row.get(0)
        })
    }

    /// Deletes a project's rows, and its chats', but nothing on disk.
    pub fn remove_project(&self, id: i64) -> Result<()> {
        self.delete_where("DELETE FROM projects WHERE id = ?1", id)
    }

    pub fn add_chat(&self, project_id: i64, agent: Agent) -> Result<i64> {
        let now = now();
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let id: i64 = tx.query_row(
            "UPDATE sequences
             SET value = MAX(value, COALESCE((SELECT MAX(id) FROM chats), 0)) + 1
             WHERE name = 'chat' RETURNING value",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO chats (id, project_id, agent, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, project_id, agent.label(), now],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn delete_chat(&self, id: i64) -> Result<()> {
        self.delete_where("DELETE FROM chats WHERE id = ?1", id)
    }

    /// Runs a delete, then deletes the sessions no chat refers to anymore.
    fn delete_where(&self, sql: &str, id: i64) -> Result<()> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        tx.execute(sql, [id])?;
        tx.execute(
            "DELETE FROM sessions WHERE id NOT IN (SELECT session_id FROM chat_sessions)",
            [],
        )?;
        tx.commit()
    }

    /// A chat's agent and its project's folder.
    pub fn chat(&self, id: i64) -> Result<Option<(Agent, PathBuf)>> {
        self.read()
            .query_row(
                "SELECT c.agent, p.path FROM chats c JOIN projects p ON p.id = c.project_id
                 WHERE c.id = ?1",
                [id],
                |row| {
                    Ok((
                        agent(&row.get::<_, String>(0)?)?,
                        PathBuf::from(row.get::<_, String>(1)?),
                    ))
                },
            )
            .optional()
    }

    /// A chat's sessions, in order.
    pub fn sessions(&self, chat_id: i64) -> Result<Vec<Session>> {
        let conn = self.read();
        let mut stmt = conn.prepare(&format!(
            "SELECT s.id, s.agent_id, s.transcript_path, {TITLE}, s.prompted,
                    s.started_at, s.missing, cs.reason, cs.ended_reason
             FROM chat_sessions cs JOIN sessions s ON s.id = cs.session_id
             WHERE cs.chat_id = ?1 ORDER BY cs.position"
        ))?;
        stmt.query_map([chat_id], |row| {
            Ok(Session {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                transcript_path: PathBuf::from(row.get::<_, String>(2)?),
                title: row.get(3)?,
                prompted: row.get(4)?,
                started_at: row.get(5)?,
                missing: row.get(6)?,
                reason: row.get(7)?,
                ended: row.get(8)?,
            })
        })?
        .collect()
    }

    /// Records that a chat's CLI is in a session, for the given reason. A
    /// session the chat was already in moves to the end, so each one shows
    /// once, in the order it was last used.
    pub fn record_session(
        &self,
        chat_id: i64,
        agent: Agent,
        agent_id: &str,
        transcript_path: &Path,
        reason: Option<&str>,
    ) -> Result<Recorded> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sessions (id, agent, agent_id, transcript_path, started_at)
             VALUES (
                 COALESCE(
                     (SELECT MAX(value, COALESCE((SELECT MAX(id) FROM sessions), 0)) + 1
                      FROM sequences WHERE name = 'session'),
                     (SELECT COALESCE(MAX(id), 0) + 1 FROM sessions)
                 ),
                 ?1, ?2, ?3, ?4
             )
             -- A session can be reported at a new path, as when Pi imports a
             -- transcript under another name. Later reads must follow it.
             ON CONFLICT (agent, agent_id)
             DO UPDATE SET transcript_path = excluded.transcript_path",
            params![
                agent.label(),
                agent_id,
                transcript_path.to_string_lossy(),
                now()
            ],
        )?;
        // Keep the sequence ahead of every id, including ones an older
        // database allocated before it existed.
        tx.execute(
            "UPDATE sequences
             SET value = MAX(value, COALESCE((SELECT MAX(id) FROM sessions), 0))
             WHERE name = 'session'",
            [],
        )?;
        let session_id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE agent = ?1 AND agent_id = ?2",
            params![agent.label(), agent_id],
            |row| row.get(0),
        )?;
        let last: Option<(i64, i64)> = tx
            .query_row(
                "SELECT session_id, position FROM chat_sessions WHERE chat_id = ?1
                 ORDER BY position DESC LIMIT 1",
                [chat_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        // Compacting a Claude session, or reloading pi, stays in the session.
        // Its chat still counts as active, which orders the sidebar.
        tx.execute(
            "UPDATE chats SET updated_at = ?2 WHERE id = ?1",
            params![chat_id, now()],
        )?;
        if last.is_some_and(|(last, _)| last == session_id) {
            tx.commit()?;
            return Ok(Recorded {
                session_id,
                left: None,
            });
        }
        tx.execute(
            "INSERT INTO chat_sessions
                 (chat_id, session_id, position, reason, ended_reason, first_position)
             VALUES (
                 ?1, ?2, ?3, ?4, NULL,
                 COALESCE((SELECT MAX(first_position) + 1 FROM chat_sessions
                           WHERE chat_id = ?1), 0)
             )
             ON CONFLICT (chat_id, session_id)
             DO UPDATE SET position = excluded.position, reason = excluded.reason,
                           ended_reason = NULL",
            params![
                chat_id,
                session_id,
                last.map_or(0, |(_, position)| position + 1),
                reason
            ],
        )?;
        if let Some((left, _)) = last {
            tx.execute(
                "UPDATE chat_sessions SET ended_reason = ?3
                 WHERE chat_id = ?1 AND session_id = ?2",
                params![chat_id, left, reason],
            )?;
        }
        tx.commit()?;
        Ok(Recorded {
            session_id,
            left: last.map(|(last, _)| last),
        })
    }

    /// Whether any of a chat's sessions has a prompt.
    #[cfg(test)]
    pub fn has_prompts(&self, chat_id: i64) -> Result<bool> {
        self.read().query_row(
            "SELECT EXISTS (
                SELECT 1 FROM chat_sessions cs JOIN sessions s ON s.id = cs.session_id
                WHERE cs.chat_id = ?1 AND s.prompted
            )",
            [chat_id],
            |row| row.get(0),
        )
    }

    pub fn transcript(&self, session_id: i64) -> Result<Option<Transcript>> {
        self.read()
            .query_row(
                "SELECT agent, agent_id, transcript_path, read_offset FROM sessions WHERE id = ?1",
                [session_id],
                |row| {
                    Ok(Transcript {
                        agent: agent(&row.get::<_, String>(0)?)?,
                        agent_id: row.get(1)?,
                        path: PathBuf::from(row.get::<_, String>(2)?),
                        offset: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()
    }

    /// Saves a read, saying whether another read got there first. A caller
    /// that does is expected to read again from the committed offset, so that
    /// messages another read captured fewer of aren't lost.
    pub(crate) fn save_read_state(
        &self,
        session_id: i64,
        from: u64,
        read: &transcript::Read,
    ) -> Result<Saved> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        // The session can be gone, deleted with its chat while its transcript
        // was being parsed.
        let stored: Option<(i64, Option<i64>)> = tx
            .query_row(
                "SELECT read_offset, leaf_item_id FROM sessions WHERE id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((offset, stored_leaf)) = stored else {
            return Ok(Saved::Gone);
        };
        if offset as u64 != from {
            return Ok(Saved::Stale);
        }
        let mut changed = false;
        if read.reset {
            // A notice is agentshub's own, and outlives the transcript it is
            // about, such as why the session couldn't be resumed.
            tx.execute(
                "DELETE FROM items WHERE session_id = ?1 AND kind <> ?2",
                params![session_id, Kind::Notice.as_str()],
            )?;
            tx.execute("DELETE FROM lines WHERE session_id = ?1", [session_id])?;
            tx.execute(
                "UPDATE sessions SET prompt = NULL, prompted = 0, custom_title = NULL,
                        generated_title = NULL
                 WHERE id = ?1",
                [session_id],
            )?;
            changed = true;
        }

        // The kept items this session already has, by the id of their
        // transcript line. A new item's parent is the nearest kept ancestor of
        // its line: the line itself if it kept one, or one further back
        // through the lines that kept none.
        let mut kept: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT source_id, id FROM items WHERE session_id = ?1 AND source_id IS NOT NULL
                 UNION ALL
                 SELECT source_id, item_id FROM lines
                 WHERE session_id = ?1 AND item_id IS NOT NULL",
            )?;
            for row in stmt.query_map([session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (source_id, id) = row?;
                kept.insert(source_id, id);
            }
        }

        // Claude can append an old line again on resume. Then the last entry
        // isn't a new leaf: the session stayed on the branch it was already
        // on, so its leaf must not move back to that line.
        let mut fresh_lines = HashSet::new();
        let mut last_fresh_item = None;
        let mut insert = tx.prepare(
            "INSERT INTO items (session_id, source_id, parent_item_id, kind, at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT DO NOTHING",
        )?;
        for item in &read.items {
            let parent_item = item
                .parent_id
                .as_deref()
                .and_then(|parent| ancestor(&kept, &read.parents, parent));
            let inserted = insert.execute(params![
                session_id,
                item.source_id,
                parent_item,
                item.kind.as_str(),
                item.at,
                item.payload.to_string()
            ])?;
            if inserted > 0 {
                changed = true;
                if let Some(source_id) = &item.source_id {
                    kept.insert(source_id.clone(), tx.last_insert_rowid());
                    fresh_lines.insert(source_id.clone());
                    last_fresh_item = Some(source_id.clone());
                }
            }
        }
        drop(insert);
        let mut complete = tx.prepare(
            "UPDATE items SET payload = json_set(payload,
                '$.output', ?3, '$.truncated', json(?4), '$.error', json(?5))
             WHERE session_id = ?1 AND kind = 'tool'
               AND json_extract(payload, '$.id') = ?2",
        )?;
        for output in &read.outputs {
            changed |= complete.execute(params![
                session_id,
                output.call_id,
                output.output,
                output.truncated.to_string(),
                output.error.to_string()
            ])? > 0;
        }
        drop(complete);

        // A line that kept no item still has to say where it sits, because a
        // later read can hang off it: the next message often has one of these
        // lines as its parent. Its nearest kept ancestor is enough.
        {
            let mut line = tx.prepare(
                "INSERT INTO lines (session_id, source_id, item_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT DO NOTHING",
            )?;
            for id in &read.lines {
                if kept.contains_key(id) {
                    continue;
                }
                let item = ancestor(&kept, &read.parents, id);
                line.execute(params![session_id, id, item])?;
            }
        }

        // The newest line that wasn't a replay decides the branch. A line that
        // kept no item still decides it, as pi's `/tree` does with a branch
        // summary.
        let mut seen = HashSet::new();
        let mut newest = None;
        for id in &read.lines {
            if !seen.insert(id.as_str()) {
                continue;
            }
            // A line is fresh if it kept an item now, or if it kept none: the
            // database never stored it, so this read is where it counts.
            if fresh_lines.contains(id) || !kept.contains_key(id) {
                newest = Some(id.clone());
            }
        }
        if read.lines.is_empty() {
            // A read built by hand rather than parsed, as tests do, has no
            // line order of its own. A fresh line that is the leaf is a branch
            // change; otherwise the last fresh item is the newest.
            let leaf_is_fresh = read.leaf.as_deref().is_some_and(|leaf| {
                read.parents.contains_key(leaf)
                    && !kept.contains_key(leaf)
                    && !fresh_lines.contains(leaf)
            });
            newest = if leaf_is_fresh {
                read.leaf.clone()
            } else {
                last_fresh_item
            };
        }
        let update_leaf = read.reset || newest.is_some();
        let leaf = newest
            .as_deref()
            .and_then(|line| ancestor(&kept, &read.parents, line));

        // A line that kept no item can still change the branch the session is
        // on, as pi's `/tree` does with a branch summary. If it doesn't extend
        // the branch the session was on, the history changed.
        if !changed
            && update_leaf
            && leaf != stored_leaf
            && let Some(leaf) = leaf
            && !extends_branch(&tx, session_id, stored_leaf, leaf)?
        {
            changed = true;
        }

        if let Some(title) = &read.custom_title {
            tx.execute(
                "UPDATE sessions SET custom_title = ?2 WHERE id = ?1",
                params![session_id, title],
            )?;
        }
        if let Some(title) = &read.generated_title {
            tx.execute(
                "UPDATE sessions SET generated_title = ?2 WHERE id = ?1",
                params![session_id, title],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET
                 prompt = COALESCE(prompt, ?2),
                 prompted = MAX(prompted, ?3),
                 read_offset = ?4,
                 missing = 0,
                 leaf_item_id = CASE
                     WHEN ?5 THEN ?6
                     ELSE leaf_item_id
                 END
             WHERE id = ?1",
            params![
                session_id,
                read.prompt,
                read.prompted,
                read.offset as i64,
                update_leaf,
                leaf,
            ],
        )?;
        tx.commit()?;
        Ok(if changed {
            Saved::Changed
        } else {
            Saved::Unchanged
        })
    }

    /// Marks a session's transcript as gone, and says whether it wasn't yet.
    pub fn set_missing(&self, session_id: i64) -> Result<bool> {
        let changed = self.write().execute(
            "UPDATE sessions SET missing = 1 WHERE id = ?1 AND missing = 0",
            [session_id],
        )?;
        Ok(changed > 0)
    }

    /// Adds something agentshub says to a session's items.
    pub fn add_notice(&self, session_id: i64, text: &str) -> Result<()> {
        self.write().execute(
            "INSERT INTO items (session_id, kind, at, payload) VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id,
                Kind::Notice.as_str(),
                now(),
                json!({ "text": text }).to_string()
            ],
        )?;
        Ok(())
    }

    /// Marks the chats that went through a session as active, which orders the
    /// sidebar. Browsing history must not call this.
    pub fn touch_session(&self, session_id: i64) -> Result<()> {
        self.write().execute(
            "UPDATE chats SET updated_at = ?2 WHERE id IN
                (SELECT chat_id FROM chat_sessions WHERE session_id = ?1)",
            params![session_id, now()],
        )?;
        Ok(())
    }

    /// Writes a consistent copy of the database with SQLite's own `VACUUM
    /// INTO`, which leaves the original alone.
    pub fn export(&self, path: &Path) -> anyhow::Result<()> {
        let conn = self.write();
        fs::remove_file(path).ok();
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
            .with_context(|| format!("can't write {}", path.display()))?;
        restrict(path);
        Ok(())
    }

    /// A session's last `limit` items, in conversation order. Of a session
    /// that branched, only the branch it ended on: the one its leaf is on.
    /// Notices belong to no branch, so they always show.
    pub fn items(&self, session_id: i64, limit: usize) -> Result<Vec<Item>> {
        let conn = self.read();
        let leaf: Option<i64> = conn
            .query_row(
                "SELECT leaf_item_id FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let mut items = match leaf {
            Some(leaf) => {
                let mut stmt = conn.prepare(
                    "WITH RECURSIVE branch(id, parent) AS (
                         SELECT id, parent_item_id FROM items WHERE id = ?1
                         UNION
                         SELECT i.id, i.parent_item_id
                         FROM items i JOIN branch b ON i.id = b.parent
                     )
                     SELECT id, kind, payload FROM items
                     WHERE id IN (SELECT id FROM branch)
                        OR (session_id = ?2 AND kind = 'notice')
                     ORDER BY id DESC LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![leaf, session_id, limit as i64], item_row)?;
                rows.collect::<Result<Vec<_>>>()?
            }
            // Codex transcripts don't branch, and their items have no line ids
            // to hang off, so they're simply kept in order.
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, kind, payload FROM items
                     WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![session_id, limit as i64], item_row)?;
                rows.collect::<Result<Vec<_>>>()?
            }
        };
        items.reverse();
        Ok(items.into_iter().flatten().collect())
    }
}

/// How long to wait for another process's write before giving up.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn set_pragmas(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // WAL, and no fsync per commit: a commit costs no more than the WAL write.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
}

fn create_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Keeps the transcripts in the database to its user.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions).ok();
    }
}

#[cfg(not(unix))]
fn restrict(_: &Path) {}

/// A panic while holding the connection shouldn't take every later query with
/// it.
fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|err| err.into_inner())
}

/// Rows only hold agents this version wrote, and one from a newer version is
/// refused when the database is opened.
fn agent(label: &str) -> Result<Agent> {
    Agent::from_label(label).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown agent {label:?}"),
            )),
        )
    })
}

/// Whether `old`, the leaf the session was on, is still on the branch that
/// `new` ends. A line that only extends the branch adds nothing that was
/// hidden, so it doesn't change what the history shows.
fn extends_branch(
    tx: &rusqlite::Transaction<'_>,
    session_id: i64,
    old: Option<i64>,
    new: i64,
) -> Result<bool> {
    let Some(old) = old else {
        return Ok(false);
    };
    let mut next = Some(new);
    let mut seen = HashSet::new();
    while let Some(id) = next {
        if id == old {
            return Ok(true);
        }
        if !seen.insert(id) {
            return Ok(false);
        }
        next = tx
            .query_row(
                "SELECT parent_item_id FROM items WHERE id = ?1 AND session_id = ?2",
                params![id, session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
    }
    Ok(false)
}

/// The nearest kept item above a transcript line: the line itself if it kept
/// one, or the first kept ancestor through the lines between that kept none.
fn ancestor(
    kept: &HashMap<String, i64>,
    parents: &HashMap<String, Option<String>>,
    start: &str,
) -> Option<i64> {
    let mut current = Some(start);
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if let Some(&item) = kept.get(id) {
            return Some(item);
        }
        if !seen.insert(id) {
            return None;
        }
        current = parents.get(id).and_then(|parent| parent.as_deref());
    }
    None
}

fn item_row(row: &rusqlite::Row<'_>) -> Result<Option<Item>> {
    let kind: String = row.get(1)?;
    let Some(kind) = Kind::parse(&kind) else {
        return Ok(None);
    };
    Ok(Some(Item {
        id: row.get(0)?,
        kind,
        payload: serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default(),
    }))
}

/// Copies a database about to be migrated, since what it holds can't be
/// recovered anywhere else.
fn backup(conn: &Connection, path: &Path, version: i64) -> anyhow::Result<()> {
    let backup = PathBuf::from(format!("{}.v{version}.bak", path.display()));
    fs::remove_file(&backup).ok();
    conn.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
        .with_context(|| format!("can't back up the database to {}", backup.display()))?;
    restrict(&backup);
    Ok(())
}

/// Schema 5: each item keeps the id of its nearest kept ancestor, each session
/// the item its branch ends on, and each line that kept no item the ancestor it
/// hangs off. A branch then follows item ids instead of walking the line tree,
/// which a later read can still hang off.
fn resolve_ancestry(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE items ADD COLUMN parent_item_id INTEGER
             REFERENCES items (id) ON DELETE CASCADE;
         ALTER TABLE sessions ADD COLUMN leaf_item_id INTEGER
             REFERENCES items (id) ON DELETE SET NULL;
         CREATE TABLE lines (
             session_id INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
             source_id  TEXT NOT NULL,
             item_id    INTEGER REFERENCES items (id) ON DELETE CASCADE,
             PRIMARY KEY (session_id, source_id)
         ) WITHOUT ROWID;",
    )?;
    let sessions: Vec<(i64, Option<String>)> = {
        let mut stmt = tx.prepare("SELECT id, leaf FROM sessions")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>>>()?
    };
    for (session_id, leaf) in sessions {
        let mut parents: HashMap<String, Option<String>> = HashMap::new();
        let mut dropped: Vec<String> = Vec::new();
        {
            let mut stmt =
                tx.prepare("SELECT source_id, parent_id FROM links WHERE session_id = ?1")?;
            for row in stmt.query_map([session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })? {
                let (id, parent) = row?;
                parents.insert(id.clone(), parent);
                dropped.push(id);
            }
        }
        let items: Vec<(i64, Option<String>, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT id, source_id, parent_id FROM items
                 WHERE session_id = ?1 ORDER BY id",
            )?;
            let rows = stmt.query_map([session_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<Result<Vec<_>>>()?
        };
        // A line that kept items owns its ancestry, which takes precedence
        // over its link, as it did when the branch was shown.
        let mut kept: HashMap<String, i64> = HashMap::new();
        for (id, source_id, parent_id) in &items {
            if let Some(source_id) = source_id {
                kept.insert(source_id.clone(), *id);
                parents.insert(source_id.clone(), parent_id.clone());
            }
        }
        for (id, _, parent_id) in &items {
            let parent = parent_id
                .as_deref()
                .and_then(|parent| ancestor(&kept, &parents, parent));
            tx.execute(
                "UPDATE items SET parent_item_id = ?2 WHERE id = ?1",
                params![id, parent],
            )?;
        }
        for source_id in dropped {
            let item = ancestor(&kept, &parents, &source_id);
            tx.execute(
                "INSERT INTO lines (session_id, source_id, item_id) VALUES (?1, ?2, ?3)",
                params![session_id, source_id, item],
            )?;
        }
        if let Some(leaf) = leaf
            && let Some(leaf) = ancestor(&kept, &parents, &leaf)
        {
            tx.execute(
                "UPDATE sessions SET leaf_item_id = ?2 WHERE id = ?1",
                params![session_id, leaf],
            )?;
        }
    }
    // Databases from before the leaf was kept end on their last item. Codex
    // has no line ids to branch on, so its sessions keep the linear fallback.
    tx.execute_batch(
        "UPDATE sessions SET leaf_item_id = (
             SELECT MAX(i.id) FROM items i
             WHERE i.session_id = sessions.id AND i.kind <> 'notice'
         )
         WHERE leaf_item_id IS NULL
           AND EXISTS (
               SELECT 1 FROM items i
               WHERE i.session_id = sessions.id AND i.source_id IS NOT NULL
           );
         DROP TABLE links;
         ALTER TABLE items DROP COLUMN parent_id;
         ALTER TABLE sessions DROP COLUMN leaf;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(db: &Db) -> i64 {
        let project = db.add_project(Path::new("/home/me/project")).unwrap();
        db.add_chat(project, Agent::Claude).unwrap()
    }

    fn record(db: &Db, chat: i64, id: &str, reason: &str) -> Recorded {
        db.record_session(
            chat,
            Agent::Claude,
            id,
            Path::new(&format!("/t/{id}.jsonl")),
            Some(reason),
        )
        .unwrap()
    }

    fn order(db: &Db, chat: i64) -> Vec<(String, Option<String>)> {
        db.sessions(chat)
            .unwrap()
            .into_iter()
            .map(|session| (session.agent_id, session.reason))
            .collect()
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn upgrades_a_database_from_the_first_schema() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1i64).unwrap();
        }

        let db = Db::open(&path).unwrap();
        let chat = chat(&db);
        record(&db, chat, "a", "startup");
        record(&db, chat, "b", "clear");
        let ended: Vec<Option<String>> = db
            .sessions(chat)
            .unwrap()
            .into_iter()
            .map(|session| session.ended)
            .collect();
        assert_eq!(ended, [Some("clear".into()), None]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Builds the items of one read, each line one item, chained.
    fn chain(first: usize, last: usize, prefix: &str, parent: Option<&str>) -> transcript::Read {
        let mut read = transcript::Read::default();
        let mut parent = parent.map(String::from);
        for id in first..=last {
            let id = id.to_string();
            read.items.push(transcript::Item {
                source_id: Some(id.clone()),
                parent_id: parent.clone(),
                kind: Kind::User,
                at: None,
                payload: json!({ "text": format!("{prefix}{id}") }),
            });
            read.parents.insert(id.clone(), parent.clone());
            read.lines.push(id.clone());
            parent = Some(id);
        }
        read.leaf = parent;
        read
    }

    #[test]
    fn a_read_can_hang_off_a_line_the_read_before_dropped() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;

        // A kept line, then a line that keeps no item, which the read ends on.
        let mut read = transcript::Read::default();
        read.offset = 20;
        read.items.push(transcript::Item {
            source_id: Some("1".into()),
            parent_id: None,
            kind: Kind::User,
            at: None,
            payload: json!({ "text": "first" }),
        });
        read.parents.insert("1".into(), None);
        read.parents.insert("2".into(), Some("1".into()));
        read.lines.extend(["1".into(), "2".into()]);
        read.leaf = Some("2".into());
        assert!(db.save_read_state(session, 0, &read).unwrap().changed());

        // The next message's parent is that dropped line.
        let mut read = transcript::Read::default();
        read.offset = 40;
        read.items.push(transcript::Item {
            source_id: Some("3".into()),
            parent_id: Some("2".into()),
            kind: Kind::User,
            at: None,
            payload: json!({ "text": "second" }),
        });
        read.parents.insert("3".into(), Some("2".into()));
        read.lines.push("3".into());
        read.leaf = Some("3".into());
        assert!(db.save_read_state(session, 20, &read).unwrap().changed());

        let texts: Vec<String> = db
            .items(session, 100)
            .unwrap()
            .into_iter()
            .map(|item| item.payload["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, ["first", "second"]);
    }

    #[test]
    fn items_page_from_the_end_of_the_branch() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;
        let mut read = chain(1, 5, "", None);
        read.offset = 10;
        db.save_read_state(session, 0, &read).unwrap();

        let texts = |limit| -> Vec<String> {
            db.items(session, limit)
                .unwrap()
                .into_iter()
                .map(|item| item.payload["text"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(texts(2), ["4", "5"]);
        assert_eq!(texts(10).len(), 5);
    }

    #[test]
    fn exports_a_consistent_copy() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let db = Db::open(&path).unwrap();
        let chat = chat(&db);
        record(&db, chat, "a", "startup");
        let copy = dir.join("copy.db");

        db.export(&copy).unwrap();
        let exported = Db::open(&copy).unwrap();
        assert_eq!(exported.projects().unwrap().len(), 1);
        assert_eq!(exported.sessions(chat).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_a_database_from_a_newer_version() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99i64)
            .unwrap();
        let err = Db::open(&path)
            .err()
            .expect("a newer database opened")
            .to_string();
        assert!(err.contains("newer"), "{err}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chat_ids_are_not_recycled_after_deleting_a_chat_or_project() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let first;
        {
            let db = Db::open(&path).unwrap();
            first = chat(&db);
            db.delete_chat(first).unwrap();
        }
        // The sequence survives closing the database even when no chats do.
        let db = Db::open(&path).unwrap();
        let second = chat(&db);
        assert!(second > first);
        assert!(db.chat(first).unwrap().is_none());
        db.remove_project(db.projects().unwrap()[0].id).unwrap();
        let third = chat(&db);
        assert!(third > second);
        assert!(db.chat(second).unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn seeds_the_chat_sequence_from_existing_chats_during_upgrade() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        {
            let conn = Connection::open(&path).unwrap();
            for migration in &MIGRATIONS[..3] {
                conn.execute_batch(migration).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO projects VALUES (1, '/home/me/project', 'project', 0);
                 INSERT INTO chats VALUES (42, 1, 'claude', NULL, 0, 0);
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        db.delete_chat(42).unwrap();
        assert!(db.add_chat(1, Agent::Claude).unwrap() > 42);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_tool_outputs_through_their_index() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.write();
        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN UPDATE items SET payload = json_set(payload,
                    '$.output', ?3, '$.truncated', json(?4), '$.error', json(?5))
                 WHERE session_id = ?1 AND kind = 'tool'
                   AND json_extract(payload, '$.id') = ?2",
            )
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params![1i64, "call", "out", "false", "false"], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .map(|line| line.unwrap())
            .collect();
        assert!(
            plan.iter().any(|line| line.contains("items_tool_call")),
            "{plan:?}"
        );
    }

    #[test]
    fn adds_each_folder_once() {
        let db = Db::open_in_memory().unwrap();
        let first = db.add_project(Path::new("/home/me/project")).unwrap();
        assert_eq!(
            db.add_project(Path::new("/home/me/project")).unwrap(),
            first
        );
        db.add_project(Path::new("/home/me/other")).unwrap();
        let projects = db.projects().unwrap();
        assert_eq!(
            projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["other", "project"]
        );
    }

    #[test]
    fn orders_sessions_by_when_they_were_last_used() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let a = record(&db, chat, "a", "startup");
        assert_eq!(a.left, None);
        // `/compact` stays in the session.
        assert_eq!(record(&db, chat, "a", "compact"), a);
        let b = record(&db, chat, "b", "clear");
        assert_eq!(b.left, Some(a.session_id));
        record(&db, chat, "c", "new");
        // `/resume` goes back to the first one.
        assert_eq!(record(&db, chat, "a", "resume").left, Some(3));
        assert_eq!(
            order(&db, chat),
            [
                ("b".into(), Some("clear".into())),
                ("c".into(), Some("new".into())),
                ("a".into(), Some("resume".into())),
            ]
        );
        // A session says why the chat left it, and re-entering it forgets
        // that.
        let ended: Vec<Option<String>> = db
            .sessions(chat)
            .unwrap()
            .into_iter()
            .map(|session| session.ended)
            .collect();
        assert_eq!(ended, [Some("new".into()), Some("resume".into()), None]);
    }

    #[test]
    fn follows_a_known_session_to_a_new_transcript_path() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup");
        let moved = Path::new("/t/moved.jsonl");

        let recorded = db
            .record_session(chat, Agent::Claude, "a", moved, Some("resume"))
            .unwrap();
        assert_eq!(recorded.session_id, session.session_id);
        assert_eq!(
            db.transcript(session.session_id).unwrap().unwrap().path,
            moved
        );
    }

    #[test]
    fn a_read_that_loses_the_offset_race_is_stale() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;

        let item = |source_id: &str, parent_id: Option<&str>, text: &str| transcript::Item {
            source_id: Some(source_id.into()),
            parent_id: parent_id.map(String::from),
            kind: Kind::User,
            at: None,
            payload: json!({ "text": text }),
        };

        // A short read commits first.
        let mut short = transcript::Read::default();
        short.offset = 10;
        short.items.push(item("1", None, "first"));
        short.leaf = Some("1".into());
        short.prompt = Some("first".into());
        short.prompted = true;
        assert_eq!(
            db.save_read_state(session, 0, &short).unwrap(),
            Saved::Changed
        );

        // A longer read started at the same offset, but must not discard what
        // it saw after the short one's end.
        let mut long = transcript::Read::default();
        long.offset = 30;
        long.items.push(item("1", None, "first"));
        long.items.push(item("2", Some("1"), "second"));
        long.leaf = Some("2".into());
        assert_eq!(db.save_read_state(session, 0, &long).unwrap(), Saved::Stale);

        // Reading again from the committed offset keeps the second message.
        let mut retry = transcript::Read::default();
        retry.offset = 30;
        retry.items.push(item("2", Some("1"), "second"));
        retry.leaf = Some("2".into());
        assert_eq!(
            db.save_read_state(session, short.offset, &retry).unwrap(),
            Saved::Changed
        );
        let texts: Vec<String> = db
            .items(session, 100)
            .unwrap()
            .into_iter()
            .map(|item| item.payload["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, ["first", "second"]);
    }

    #[test]
    fn a_replayed_line_does_not_move_the_leaf_back() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;
        let item = |source_id: &str, parent_id: Option<&str>, text: &str| transcript::Item {
            source_id: Some(source_id.into()),
            parent_id: parent_id.map(String::from),
            kind: Kind::User,
            at: None,
            payload: json!({ "text": text }),
        };

        // The newest line is the second message, and the last line is a
        // replay of the first one.
        let mut read = transcript::Read::default();
        read.offset = 30;
        read.items.push(item("1", None, "first"));
        read.items.push(item("2", Some("1"), "second"));
        read.items.push(item("1", None, "first"));
        read.leaf = Some("1".into());
        assert!(db.save_read_state(session, 0, &read).unwrap().changed());

        let texts: Vec<String> = db
            .items(session, 100)
            .unwrap()
            .into_iter()
            .map(|item| item.payload["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, ["first", "second"]);
    }

    #[test]
    fn a_link_that_changes_the_branch_says_the_history_changed() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;
        let item = |source_id: &str, parent_id: Option<&str>, text: &str| transcript::Item {
            source_id: Some(source_id.into()),
            parent_id: parent_id.map(String::from),
            kind: Kind::User,
            at: None,
            payload: json!({ "text": text }),
        };

        let mut read = transcript::Read::default();
        read.offset = 10;
        read.items.push(item("a", None, "first"));
        read.items.push(item("b", Some("a"), "abandoned"));
        read.leaf = Some("b".into());
        assert_eq!(
            db.save_read_state(session, 0, &read).unwrap(),
            Saved::Changed
        );

        // Pi's `/tree` appends only a branch summary, which moves the leaf
        // back and hides the abandoned branch.
        let mut read = transcript::Read::default();
        read.offset = 20;
        read.parents.insert("s".into(), Some("a".into()));
        read.leaf = Some("s".into());
        assert_eq!(
            db.save_read_state(session, 10, &read).unwrap(),
            Saved::Changed
        );
        let texts: Vec<String> = db
            .items(session, 100)
            .unwrap()
            .into_iter()
            .map(|item| item.payload["text"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, ["first"]);
    }

    #[test]
    fn deleting_a_chat_keeps_the_sessions_another_chat_has() {
        let db = Db::open_in_memory().unwrap();
        let first = chat(&db);
        let second = chat(&db);
        let shared = record(&db, first, "shared", "startup").session_id;
        let own = record(&db, first, "own", "clear").session_id;
        record(&db, second, "shared", "resume");
        db.add_notice(own, "hello").unwrap();

        db.delete_chat(first).unwrap();
        assert!(db.transcript(shared).unwrap().is_some());
        assert!(db.transcript(own).unwrap().is_none());
        assert!(db.items(own, 100).unwrap().is_empty());
        assert_eq!(
            order(&db, second),
            [("shared".into(), Some("resume".into()))]
        );

        db.remove_project(db.projects().unwrap()[0].id).unwrap();
        assert!(db.transcript(shared).unwrap().is_none());
        assert!(db.projects().unwrap().is_empty());
    }

    /// Saves a first prompt for a session that has not been read.
    fn save_prompt_for_title_test(db: &Db, session: i64, prompt: &str) {
        let transcript = db.transcript(session).unwrap().unwrap();
        let mut read = transcript::Read::default();
        read.prompt = Some(prompt.into());
        read.prompted = true;
        read.offset = transcript.offset;
        db.save_read_state(session, transcript.offset, &read)
            .unwrap();
    }

    #[test]
    fn a_stale_read_does_not_fill_a_new_session_that_reuses_its_id() {
        let db = Db::open_in_memory().unwrap();
        let first = chat(&db);
        let old = record(&db, first, "old", "startup").session_id;
        db.delete_chat(first).unwrap();

        let second = chat(&db);
        let new = record(&db, second, "new", "startup").session_id;
        // The id must not be recycled, so the read that started before the
        // first chat was deleted can't find the second chat's session.
        assert_ne!(old, new);

        let mut stale = transcript::Read::default();
        stale.prompt = Some("deleted".into());
        stale.prompted = true;
        stale.items.push(transcript::Item {
            source_id: Some("1".into()),
            parent_id: None,
            kind: Kind::User,
            at: None,
            payload: json!({ "text": "deleted" }),
        });
        assert!(!db.save_read_state(old, 0, &stale).unwrap().changed());
        assert!(db.items(new, 100).unwrap().is_empty());
        assert_eq!(db.transcript(new).unwrap().unwrap().offset, 0);
        assert!(!db.has_prompts(second).unwrap());
    }

    #[test]
    fn titles_a_chat_after_its_own_first_session() {
        let db = Db::open_in_memory().unwrap();
        let first = chat(&db);
        let shared = record(&db, first, "shared", "startup").session_id;
        save_prompt_for_title_test(&db, shared, "the first chat's title");

        let second = chat(&db);
        let own = record(&db, second, "own", "startup").session_id;
        save_prompt_for_title_test(&db, own, "the second chat's title");
        record(&db, second, "shared", "resume");

        let projects = db.projects().unwrap();
        let title = projects
            .iter()
            .flat_map(|project| &project.chats)
            .find(|chat| chat.id == second)
            .and_then(|chat| chat.title.as_deref());
        assert_eq!(title, Some("the second chat's title"));
    }
}
