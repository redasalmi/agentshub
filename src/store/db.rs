//! agentshub's own projects and chats, the agent sessions each chat went
//! through, and a trimmed copy of those sessions (see `transcript`), in SQLite.
//!
//! Writes go through one connection, and reads have a connection of their own,
//! so that the UI doesn't wait on a transcript read's transaction. The store
//! (see `store`) runs them on threads of its own.

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
use crate::worktree::Worktree;

/// Each migration takes the database from the version before it, as counted
/// by `PRAGMA user_version`, to the next.
const MIGRATIONS: [&str; 7] = [
    r#"
CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    path       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- Chat and session ids are never reused: a deferred open or a transcript read
-- that started before a deletion must not find a row created afterwards.
CREATE TABLE chats (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects ON DELETE CASCADE,
    agent      TEXT NOT NULL,
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
    -- Whether it has a prompt, which an image-only one has without a title.
    prompted        INTEGER NOT NULL DEFAULT 0,
    started_at      INTEGER NOT NULL,
    -- How far its transcript was read, and by which version of the parser.
    read_offset     INTEGER NOT NULL DEFAULT 0,
    parser_version  INTEGER NOT NULL DEFAULT 0,
    -- The last item of the branch the session ended on. Not a foreign key,
    -- like the other ids of items (see `items`).
    leaf_item_id    INTEGER,
    missing         INTEGER NOT NULL DEFAULT 0,
    UNIQUE (agent, agent_id)
);

-- The sessions of a chat, in order. The last one is resumed.
CREATE TABLE chat_sessions (
    chat_id        INTEGER NOT NULL REFERENCES chats ON DELETE CASCADE,
    session_id     INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    position       INTEGER NOT NULL,
    -- Where it first entered the chat, which its position loses when the chat
    -- resumes it again. It orders the sessions a chat is titled after.
    first_position INTEGER NOT NULL,
    -- Why the chat went into it, and why it left it for another one.
    reason         TEXT,
    ended_reason   TEXT,
    PRIMARY KEY (chat_id, session_id),
    UNIQUE (chat_id, position)
);
CREATE INDEX chat_sessions_session ON chat_sessions (session_id);

-- The trimmed copy of each session.
--
-- Items are only deleted a whole session at a time, so the ids that point at
-- them aren't foreign keys: a cascade down a long branch goes deeper than
-- SQLite allows, and every deleted item would scan for the rows naming it.
CREATE TABLE items (
    id             INTEGER PRIMARY KEY,
    session_id     INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    source_id      TEXT,
    -- The nearest kept item above its transcript line.
    parent_item_id INTEGER,
    kind           TEXT NOT NULL,
    -- A tool call's id, which its output arrives with on a later line.
    call_id        TEXT,
    at             INTEGER,
    payload        TEXT NOT NULL,
    UNIQUE (session_id, source_id)
);
CREATE INDEX items_session ON items (session_id, id);
CREATE INDEX items_tool_call ON items (session_id, call_id) WHERE call_id IS NOT NULL;

-- Transcript lines that kept no item, each with the item it hangs off. A later
-- message often has one of these lines as its parent, so a read has to know
-- where they sit even though they aren't shown.
CREATE TABLE lines (
    session_id INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    source_id  TEXT NOT NULL,
    item_id    INTEGER,
    PRIMARY KEY (session_id, source_id)
) WITHOUT ROWID;
"#,
    r#"
-- The title the user gave a chat, and where it sits in its project. Pinned
-- chats come first, and each of the two groups keeps its own order.
ALTER TABLE chats ADD COLUMN title TEXT;
ALTER TABLE chats ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chats ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
-- The chats keep the order they were shown in, most recently used first.
UPDATE chats SET position = (
    SELECT COUNT(*) FROM chats o
    WHERE o.project_id = chats.project_id
      AND (o.updated_at, o.id) > (chats.updated_at, chats.id)
);
"#,
    r#"
-- The app's settings, by name. A setting without a row has its default.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value ANY NOT NULL
) WITHOUT ROWID;
"#,
    r#"
-- Where a project sits in the rail.
ALTER TABLE projects ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
-- The projects keep the order they were shown in, newest first.
UPDATE projects SET position = (
    SELECT COUNT(*) FROM projects o
    WHERE (o.created_at, o.id) > (projects.created_at, projects.id)
);
"#,
    r#"
-- The git worktree Agents Hub made for a chat, and the folder in it the chat
-- runs in: its project's folder in that worktree. Both are set or neither is,
-- and without them the chat runs in its project's folder.
ALTER TABLE chats ADD COLUMN worktree TEXT;
ALTER TABLE chats ADD COLUMN worktree_folder TEXT;
"#,
    r#"
-- When the user archived a chat or a project, which the sidebar then leaves
-- out. The chats of an archived project go with it without being archived
-- themselves.
ALTER TABLE chats ADD COLUMN archived_at INTEGER;
ALTER TABLE projects ADD COLUMN archived_at INTEGER;
"#,
    r#"
-- What the search finds of an item: what the user and the agent said. Only a
-- tool call's payload is ever updated, so the index needs no update trigger.
ALTER TABLE items ADD COLUMN search_text TEXT GENERATED ALWAYS AS (
    CASE WHEN kind IN ('user', 'assistant') THEN json_extract(payload, '$.text') END
) VIRTUAL;
CREATE VIRTUAL TABLE items_fts USING fts5(
    search_text,
    content = 'items',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);
CREATE TRIGGER items_fts_insert AFTER INSERT ON items BEGIN
    INSERT INTO items_fts (rowid, search_text) VALUES (new.id, new.search_text);
END;
CREATE TRIGGER items_fts_delete AFTER DELETE ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, search_text)
    VALUES ('delete', old.id, old.search_text);
END;
INSERT INTO items_fts (items_fts) VALUES ('rebuild');
"#,
];

/// The schema version this build writes.
const SCHEMA: i64 = MIGRATIONS.len() as i64;

const TITLE: &str = "COALESCE(s.custom_title, s.generated_title, s.prompt)";

/// The order of a project's chats.
const CHAT_ORDER: &str = "pinned DESC, position, id DESC";

/// The order of the projects.
const PROJECT_ORDER: &str = "position, id DESC";

/// How many messages a search finds, and how many of them can be in one chat.
const SEARCH_HITS: i64 = 30;
const SEARCH_HITS_PER_CHAT: i64 = 3;

/// How many words of a message show around what a search found in it.
const SNIPPET_WORDS: i64 = 16;

/// Where the database lives: `$XDG_DATA_HOME/agentshub/agentshub.db`.
pub fn path() -> Option<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::home_dir()?.join(".local/share"),
    };
    Some(data.join("agentshub").join("agentshub.db"))
}

/// Opens the database where it lives, once this process holds the lock that
/// keeps a second Agents Hub off it, and returns that lock with it, and what
/// became of a database the user imported before the restart. The error says
/// why Agents Hub can't be used.
pub fn open_locked() -> anyhow::Result<(Db, fs::File, Option<Imported>)> {
    let path = path().context("There is no home folder to keep the database in.")?;
    // The lock comes first: a second instance must not migrate the database
    // under the first one, or replace it.
    let lock = lock_dir(path.parent().unwrap_or(&path))?;
    let imported = take_import(&path);
    let dir = path.parent().unwrap_or(&path).display().to_string();
    let db = Db::open(&path)
        .with_context(|| format!("Couldn't open the database in {dir}, so nothing can be kept"))?;
    Ok((db, lock, imported))
}

/// Takes the lock that keeps a second Agents Hub off the database in `dir`.
/// It is kept beside the database, so two copies keep each other out exactly
/// when they would share one. The kernel releases it if this process dies.
fn lock_dir(dir: &Path) -> anyhow::Result<fs::File> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Couldn't create {} to keep the database in", dir.display()))?;
    restrict(dir, 0o700);
    let path = dir.join("agentshub.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("Couldn't open the lock file {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => bail!("Agents Hub is already running."),
        Err(fs::TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("Couldn't lock {}", path.display()))
        }
    }
}

/// Seconds since the Unix epoch, which is how the database keeps time.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub id: i64,
    pub path: PathBuf,
    pub name: String,
    pub archived_at: Option<i64>,
    /// Pinned first, then in the order the user gave them.
    pub chats: Vec<Chat>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: i64,
    pub project_id: i64,
    pub agent: Agent,
    pub title: Option<String>,
    pub pinned: bool,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    /// The git worktree Agents Hub made for it, which it runs in.
    pub worktree: Option<Worktree>,
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
    /// Why the chat left it for another session: `clear`, `new`, `resume`,
    /// `fork`.
    pub ended: Option<String>,
}

/// What a read of a session's transcript starts from.
pub struct Transcript {
    pub agent: Agent,
    pub agent_id: String,
    pub path: PathBuf,
    pub offset: u64,
    /// The version of the parser that read it up to `offset`.
    pub parser: i64,
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

/// A message a search found.
pub struct Hit {
    pub chat_id: i64,
    /// Whether the user said it, rather than the agent.
    pub prompt: bool,
    /// The part of it around what was found, on one line.
    pub snippet: String,
}

#[derive(Clone)]
pub struct Db {
    write: Arc<Mutex<Connection>>,
    /// Reads get their own connection. In memory, there is only one.
    read: Option<Arc<Mutex<Connection>>>,
    /// The file it was opened from, which reads open again. None in memory.
    path: Option<PathBuf>,
    /// The folder the database is in, which exports stay out of. None in
    /// memory.
    dir: Option<PathBuf>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("can't create {} first", dir.display()))?;
            // The transcripts in it are as private as the reports are. The
            // folder covers what SQLite keeps beside the database, such as
            // its write-ahead log, and the backups. It may predate this, so
            // it is set every time.
            restrict(dir, 0o700);
        }
        let conn =
            Connection::open(path).with_context(|| format!("can't open {}", path.display()))?;
        restrict(path, 0o600);
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
        migrate(&mut conn, version)?;
        let read = match path {
            Some(path) => Some(Arc::new(Mutex::new(read_only(path)?))),
            None => None,
        };
        Ok(Self {
            write: Arc::new(Mutex::new(conn)),
            read,
            path: path.map(Path::to_path_buf),
            dir: path.and_then(|path| fs::canonicalize(path.parent()?).ok()),
        })
    }

    /// Another handle on the database, whose reads have a connection of their
    /// own. In memory, there is only one connection to share.
    pub fn reader(&self) -> anyhow::Result<Self> {
        let read = match &self.path {
            Some(path) => Some(Arc::new(Mutex::new(read_only(path)?))),
            None => None,
        };
        Ok(Self {
            read,
            ..self.clone()
        })
    }

    /// What stops the query its reads' connection runs.
    pub fn interrupt_handle(&self) -> rusqlite::InterruptHandle {
        self.read().get_interrupt_handle()
    }

    fn write(&self) -> MutexGuard<'_, Connection> {
        lock(&self.write)
    }

    fn read(&self) -> MutexGuard<'_, Connection> {
        lock(self.read.as_ref().unwrap_or(&self.write))
    }

    /// The projects, in the order the user put them in, with their chats.
    pub fn projects(&self) -> Result<Vec<Project>> {
        let conn = self.read();
        let mut chats: HashMap<i64, Vec<Chat>> = HashMap::new();
        // A chat the user didn't title is titled after the first of its
        // sessions that has a title.
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.project_id, c.agent, c.updated_at, c.pinned, c.worktree, c.worktree_folder, c.archived_at, COALESCE(c.title, (
                SELECT {TITLE} FROM chat_sessions cs JOIN sessions s ON s.id = cs.session_id
                WHERE cs.chat_id = c.id AND {TITLE} IS NOT NULL
                ORDER BY cs.first_position LIMIT 1
            ))
            FROM chats c ORDER BY {CHAT_ORDER}"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(Chat {
                id: row.get(0)?,
                project_id: row.get(1)?,
                agent: agent(&row.get::<_, String>(2)?)?,
                updated_at: row.get(3)?,
                pinned: row.get(4)?,
                worktree: match (
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ) {
                    (Some(root), Some(folder)) => Some(Worktree {
                        root: root.into(),
                        folder: folder.into(),
                    }),
                    _ => None,
                },
                archived_at: row.get(7)?,
                title: row.get(8)?,
            })
        })?;
        for chat in rows {
            let chat = chat?;
            chats.entry(chat.project_id).or_default().push(chat);
        }

        let mut stmt = conn.prepare(&format!(
            "SELECT id, path, name, archived_at FROM projects ORDER BY {PROJECT_ORDER}"
        ))?;
        stmt.query_map([], |row| {
            let id = row.get(0)?;
            Ok(Project {
                id,
                path: PathBuf::from(row.get::<_, String>(1)?),
                name: row.get(2)?,
                archived_at: row.get(3)?,
                chats: chats.remove(&id).unwrap_or_default(),
            })
        })?
        .collect()
    }

    /// Adds a folder as a project at the top of the list, or finds the
    /// project it already is, which stays where it is unless it was archived:
    /// then it comes back at the top.
    pub fn add_project(&self, path: &Path) -> Result<i64> {
        let conn = self.write();
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let path = path.to_string_lossy();
        conn.execute(
            "INSERT INTO projects (path, name, created_at, position)
             VALUES (?1, ?2, ?3, (SELECT COALESCE(MIN(position), 0) - 1 FROM projects))
             ON CONFLICT (path) DO UPDATE SET
                 archived_at = NULL,
                 position = (SELECT MIN(position) - 1 FROM projects)
             WHERE archived_at IS NOT NULL",
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

    /// Adds a chat at the top of its project's unpinned chats, running in
    /// its project's folder or in `worktree`.
    pub fn add_chat(
        &self,
        project_id: i64,
        agent: Agent,
        worktree: Option<&Worktree>,
    ) -> Result<i64> {
        insert_chat(&self.write(), project_id, agent, worktree)
    }

    /// Adds a chat as `add_chat` does, titled `title` unless it's empty,
    /// and brings its project back if it's archived, all at once. Nothing is
    /// added once the project is removed, as its id can then name another
    /// folder than `project`.
    pub fn add_chat_to(
        &self,
        project_id: i64,
        project: &Path,
        agent: Agent,
        worktree: Option<&Worktree>,
        title: &str,
    ) -> Result<Option<i64>> {
        let mut conn = self.write();
        // It reads before it writes, and a read transaction that has to
        // become a write fails at once while another process writes, rather
        // than waiting for it as the busy timeout says.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let path: Option<String> = tx
            .query_row(
                "SELECT path FROM projects WHERE id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?;
        if path.as_deref().map(Path::new) != Some(project) {
            return Ok(None);
        }
        let id = insert_chat(&tx, project_id, agent, worktree)?;
        title_chat(&tx, id, title)?;
        restore_project(&tx, project_id)?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// Titles a chat, or with an empty title, titles it after its sessions
    /// again.
    pub fn rename_chat(&self, id: i64, title: &str) -> Result<()> {
        title_chat(&self.write(), id, title)
    }

    /// Pins or unpins a chat, which goes to the top of its new group.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        self.write().execute(
            "UPDATE chats SET pinned = ?2, position = (
                SELECT MIN(position) - 1 FROM chats o WHERE o.project_id = chats.project_id
             )
             WHERE id = ?1",
            params![id, pinned],
        )?;
        Ok(())
    }

    /// Archives a chat, or takes it out of the archive where it was, as an
    /// undo does.
    pub fn set_chat_archived(&self, id: i64, archived: bool) -> Result<()> {
        self.write().execute(
            "UPDATE chats SET archived_at = ?2 WHERE id = ?1",
            params![id, archived.then(now)],
        )?;
        Ok(())
    }

    /// Archives a project, or takes it out of the archive where it was.
    pub fn set_project_archived(&self, id: i64, archived: bool) -> Result<()> {
        self.write().execute(
            "UPDATE projects SET archived_at = ?2 WHERE id = ?1",
            params![id, archived.then(now)],
        )?;
        Ok(())
    }

    /// Brings back a chat and its project, whichever is archived, each at
    /// the top of its list. Says whether either was.
    pub fn restore_chat(&self, id: i64) -> Result<bool> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let chat = tx.execute(
            "UPDATE chats SET archived_at = NULL, position = (
                SELECT MIN(position) - 1 FROM chats o WHERE o.project_id = chats.project_id
             )
             WHERE id = ?1 AND archived_at IS NOT NULL",
            [id],
        )?;
        let project = tx.execute(
            "UPDATE projects SET archived_at = NULL, position = (
                SELECT MIN(position) - 1 FROM projects
             )
             WHERE id = (SELECT project_id FROM chats WHERE id = ?1) AND archived_at IS NOT NULL",
            [id],
        )?;
        tx.commit()?;
        Ok(chat + project > 0)
    }

    /// Brings back an archived project at the top of the list. Says whether
    /// it was archived.
    pub fn restore_project(&self, id: i64) -> Result<bool> {
        restore_project(&self.write(), id)
    }

    /// The saved settings, by name. A setting without a row has its default.
    pub fn settings(&self) -> Result<HashMap<String, rusqlite::types::Value>> {
        let conn = self.read();
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }

    pub fn set_flag(&self, key: &str, value: bool) -> Result<()> {
        self.write().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_text(&self, key: &str, value: &str) -> Result<()> {
        self.write().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_number(&self, key: &str, value: f64) -> Result<()> {
        self.write().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Moves a chat to where `to` is, among the chats of its project that are
    /// pinned as it is. Nothing moves when `to` isn't one of them.
    pub fn move_chat(&self, id: i64, to: i64) -> Result<()> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let mut ids = tx
            .prepare(&format!(
                "SELECT id FROM chats
                 WHERE (project_id, pinned) = (SELECT project_id, pinned FROM chats WHERE id = ?1)
                 ORDER BY {CHAT_ORDER}"
            ))?
            .query_map([to], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>>>()?;
        let (Some(from), Some(index)) = (
            ids.iter().position(|&other| other == id),
            ids.iter().position(|&other| other == to),
        ) else {
            return Ok(());
        };
        ids.remove(from);
        ids.insert(index, id);
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE chats SET position = ?2 WHERE id = ?1",
                params![id, position as i64],
            )?;
        }
        tx.commit()
    }

    /// Moves a project to where `to` is. Nothing moves when `to` isn't a
    /// project.
    pub fn move_project(&self, id: i64, to: i64) -> Result<()> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let mut ids = tx
            .prepare(&format!("SELECT id FROM projects ORDER BY {PROJECT_ORDER}"))?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>>>()?;
        let (Some(from), Some(index)) = (
            ids.iter().position(|&other| other == id),
            ids.iter().position(|&other| other == to),
        ) else {
            return Ok(());
        };
        ids.remove(from);
        ids.insert(index, id);
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE projects SET position = ?2 WHERE id = ?1",
                params![id, position as i64],
            )?;
        }
        tx.commit()
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

    /// A project's folder, or `None` once it is removed. Project ids can be
    /// reused once removed, so a deferred action that kept one checks it
    /// still names the folder it was about.
    pub fn project_path(&self, id: i64) -> Result<Option<PathBuf>> {
        self.read()
            .query_row("SELECT path FROM projects WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0).map(PathBuf::from)
            })
            .optional()
    }

    /// A chat's agent and the folder it runs in: its worktree's, or its
    /// project's.
    pub fn chat(&self, id: i64) -> Result<Option<(Agent, PathBuf)>> {
        self.read()
            .query_row(
                "SELECT c.agent, COALESCE(c.worktree_folder, p.path)
                 FROM chats c JOIN projects p ON p.id = c.project_id
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
                    s.started_at, s.missing, cs.ended_reason
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
                ended: row.get(7)?,
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
            "INSERT INTO sessions (agent, agent_id, transcript_path, started_at)
             VALUES (?1, ?2, ?3, ?4)
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
        // Positions only grow, so a session's first one is where it first
        // entered the chat.
        tx.execute(
            "INSERT INTO chat_sessions (chat_id, session_id, position, first_position, reason)
             VALUES (?1, ?2, ?3, ?3, ?4)
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

    pub fn transcript(&self, session_id: i64) -> Result<Option<Transcript>> {
        self.read()
            .query_row(
                "SELECT agent, agent_id, transcript_path, read_offset, parser_version
                 FROM sessions WHERE id = ?1",
                [session_id],
                |row| {
                    Ok(Transcript {
                        agent: agent(&row.get::<_, String>(0)?)?,
                        agent_id: row.get(1)?,
                        path: PathBuf::from(row.get::<_, String>(2)?),
                        offset: row.get::<_, i64>(3)? as u64,
                        parser: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    /// Where every session's transcript is, how far it was read, and by which
    /// version of the parser.
    pub fn read_states(&self) -> Result<Vec<(i64, PathBuf, u64, i64)>> {
        let conn = self.read();
        let mut stmt =
            conn.prepare("SELECT id, transcript_path, read_offset, parser_version FROM sessions")?;
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
                row.get::<_, i64>(2)? as u64,
                row.get(3)?,
            ))
        })?
        .collect()
    }

    /// The messages that have everything `input` asks for, which mustn't be
    /// blank: the user's first, then the most relevant.
    pub fn search(&self, input: &str) -> Result<Vec<Hit>> {
        let conn = self.read();
        let mut stmt = conn.prepare(
            "WITH found AS (
                 SELECT f.rowid AS id, cs.chat_id, i.kind = 'user' AS prompt, f.rank AS score,
                        ROW_NUMBER() OVER (
                            PARTITION BY cs.chat_id ORDER BY i.kind = 'user' DESC, f.rank
                        ) AS nth
                 FROM items_fts f
                 JOIN items i ON i.id = f.rowid
                 JOIN chat_sessions cs ON cs.session_id = i.session_id
                 WHERE items_fts MATCH ?1
             ), kept AS (
                 SELECT * FROM found WHERE nth <= ?2 ORDER BY prompt DESC, score LIMIT ?3
             )
             SELECT k.chat_id, k.prompt, snippet(items_fts, 0, '', '', '…', ?4)
             FROM kept k JOIN items_fts ON items_fts.rowid = k.id
             WHERE items_fts MATCH ?1
             ORDER BY k.prompt DESC, k.score",
        )?;
        stmt.query_map(
            params![
                match_query(input),
                SEARCH_HITS_PER_CHAT,
                SEARCH_HITS,
                SNIPPET_WORDS
            ],
            |row| {
                Ok(Hit {
                    chat_id: row.get(0)?,
                    prompt: row.get(1)?,
                    snippet: row
                        .get::<_, String>(2)?
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                })
            },
        )?
        .collect()
    }

    /// Saves a read, and says whether the history shows anything new. The
    /// session can be gone, deleted with its chat while its transcript was
    /// being parsed; then nothing is saved.
    pub(crate) fn save_read_state(&self, session_id: i64, read: &transcript::Read) -> Result<bool> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let stored_leaf: Option<Option<i64>> = tx
            .query_row(
                "SELECT leaf_item_id FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored_leaf) = stored_leaf else {
            return Ok(false);
        };
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
        let mut insert = tx.prepare(
            "INSERT INTO items (session_id, source_id, parent_item_id, kind, call_id, at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
        )?;
        for item in &read.items {
            let parent_item = item
                .parent_id
                .as_deref()
                .and_then(|parent| ancestor(&kept, &read.parents, parent));
            let call_id = item.payload["id"]
                .as_str()
                .filter(|_| item.kind == Kind::Tool);
            let inserted = insert.execute(params![
                session_id,
                item.source_id,
                parent_item,
                item.kind.as_str(),
                call_id,
                item.at,
                item.payload.to_string()
            ])?;
            if inserted > 0 {
                changed = true;
                if let Some(source_id) = &item.source_id {
                    kept.insert(source_id.clone(), tx.last_insert_rowid());
                    fresh_lines.insert(source_id.clone());
                }
            }
        }
        drop(insert);
        let mut complete = tx.prepare(
            "UPDATE items SET payload = json_set(payload,
                '$.output', ?3, '$.truncated', json(?4), '$.error', json(?5))
             WHERE session_id = ?1 AND call_id = ?2",
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
                 parser_version = ?5,
                 missing = 0,
                 leaf_item_id = CASE
                     WHEN ?6 THEN ?7
                     ELSE leaf_item_id
                 END
             WHERE id = ?1",
            params![
                session_id,
                read.prompt,
                read.prompted,
                read.offset as i64,
                transcript::PARSER,
                update_leaf,
                leaf,
            ],
        )?;
        tx.commit()?;
        Ok(changed)
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

    /// Whether a session was prompted, and when its last prompt with a time
    /// was sent, in seconds since the Unix epoch.
    pub fn last_prompt(&self, session_id: i64) -> Result<(bool, Option<i64>)> {
        self.read().query_row(
            "SELECT prompted,
                    (SELECT MAX(at) FROM items WHERE session_id = ?1 AND kind = 'user')
             FROM sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// Writes a consistent copy of the database, outside its own folder:
    /// replacing a file there could replace the database or its write-ahead
    /// log.
    pub fn export(&self, path: &Path) -> anyhow::Result<()> {
        let target = path.parent().and_then(|dir| fs::canonicalize(dir).ok());
        if self.dir.is_some() && target == self.dir {
            bail!("choose a folder other than the one the database is in");
        }
        vacuum_into(&self.write(), path)
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

fn insert_chat(
    conn: &Connection,
    project_id: i64,
    agent: Agent,
    worktree: Option<&Worktree>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO chats (project_id, agent, created_at, updated_at, position,
                            worktree, worktree_folder)
         VALUES (?1, ?2, ?3, ?3,
                 (SELECT COALESCE(MIN(position), 0) - 1 FROM chats WHERE project_id = ?1),
                 ?4, ?5)",
        params![
            project_id,
            agent.label(),
            now(),
            worktree.map(|worktree| worktree.root.to_string_lossy()),
            worktree.map(|worktree| worktree.folder.to_string_lossy()),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn title_chat(conn: &Connection, id: i64, title: &str) -> Result<()> {
    let title = title.trim();
    conn.execute(
        "UPDATE chats SET title = ?2 WHERE id = ?1",
        params![id, (!title.is_empty()).then_some(title)],
    )?;
    Ok(())
}

fn restore_project(conn: &Connection, id: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE projects SET archived_at = NULL, position = (
            SELECT MIN(position) - 1 FROM projects
         )
         WHERE id = ?1 AND archived_at IS NOT NULL",
        [id],
    )?;
    Ok(changed > 0)
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

fn read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // Reads only wait for a writer's commit, never for each other.
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

/// Keeps the transcripts in the database to its user.
#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).ok();
}

#[cfg(not(unix))]
fn restrict(_: &Path, _: u32) {}

/// A panic while holding the lock shouldn't take every later query with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
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

/// What the user typed as an FTS5 query. Each word is quoted, as FTS5 would
/// otherwise read `foo-bar` as syntax, and the last may be half typed.
fn match_query(input: &str) -> String {
    let words: Vec<_> = input
        .split_whitespace()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect();
    format!("{}*", words.join(" "))
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

/// Takes a database from `version` to the one this build writes.
fn migrate(conn: &mut Connection, version: i64) -> Result<()> {
    for (done, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", done as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Copies a database about to be migrated, since what it holds can't be
/// recovered anywhere else.
fn backup(conn: &Connection, path: &Path, version: i64) -> anyhow::Result<()> {
    let backup = PathBuf::from(format!("{}.v{version}.bak", path.display()));
    vacuum_into(conn, &backup).context("can't back up the database")
}

/// Writes a consistent copy of the database over `path` with SQLite's own
/// `VACUUM INTO`, which leaves the original alone.
fn vacuum_into(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    fs::remove_file(path).ok();
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
        .with_context(|| format!("can't write {}", path.display()))?;
    restrict(path, 0o600);
    Ok(())
}

/// Where a database picked for import waits, beside the database, for the
/// user to see what it holds and confirm. Each pick has a copy of its own,
/// named after this and an id, so one can't delete or replace another's.
const PENDING_IMPORT: &str = "import-pending";

/// Where a confirmed import waits for the next start to put it in place of the
/// database. Nothing else may have the database open meanwhile.
const STAGED_IMPORT: &str = "import";

/// What became of a database the user imported.
pub enum Imported {
    /// It replaced the database, which was backed up to this file first, when
    /// there was one.
    Replaced(Option<PathBuf>),
    /// It couldn't, and the database is as it was.
    Failed(anyhow::Error),
}

/// A database picked for import, checked and copied beside the database,
/// waiting for the user to confirm or turn it down.
#[derive(Clone)]
pub struct PendingImport {
    copy: PathBuf,
}

impl PendingImport {
    /// Leaves the copy for the next start to put in place of the database.
    /// The last one confirmed is the one used.
    pub fn confirm(&self) -> anyhow::Result<()> {
        let path = path().context("there is no home folder to keep the database in")?;
        self.confirm_beside(&path)
    }

    fn confirm_beside(&self, path: &Path) -> anyhow::Result<()> {
        fs::rename(&self.copy, beside(path, STAGED_IMPORT))
            .context("its checked copy is gone; import it again")
    }

    /// Deletes the copy, unless it was confirmed.
    pub fn discard(&self) {
        remove_database(&self.copy);
    }
}

/// Checks that `source` is a database Agents Hub wrote, and copies it beside
/// the database, brought up to this version's schema. Returns the copy, and
/// what it holds for the user to see before confirming. The error says why it
/// can't be used.
pub fn prepare_import(source: &Path) -> anyhow::Result<(PendingImport, Vec<Project>)> {
    let path = path().context("there is no home folder to keep the database in")?;
    prepare_import_beside(&path, source)
}

fn prepare_import_beside(
    path: &Path,
    source: &Path,
) -> anyhow::Result<(PendingImport, Vec<Project>)> {
    if let (Ok(source), Ok(path)) = (fs::canonicalize(source), fs::canonicalize(path))
        && source == path
    {
        bail!("it is the database Agents Hub is using");
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        restrict(dir, 0o700);
    }
    let pending = PendingImport {
        copy: beside(
            path,
            &format!("{PENDING_IMPORT}-{}", uuid::Uuid::new_v4().simple()),
        ),
    };
    match copy_checked(source, &pending.copy) {
        Ok(projects) => Ok((pending, projects)),
        Err(err) => {
            pending.discard();
            Err(err)
        }
    }
}

/// Copies a database Agents Hub wrote to `target`, checked and migrated, and
/// lists what it holds.
fn copy_checked(source: &Path, target: &Path) -> anyhow::Result<Vec<Project>> {
    let conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("it can't be opened")?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    check_import(&conn)?;
    // A consistent copy, even of a database another program is writing.
    vacuum_into(&conn, target)?;
    drop(conn);

    let mut conn = Connection::open(target)?;
    // One file, without a write-ahead log beside it, is what gets moved in
    // place of the database.
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    migrate(&mut conn, version)?;
    let db = Db {
        write: Arc::new(Mutex::new(conn)),
        read: None,
        path: None,
        dir: None,
    };
    Ok(db.projects()?)
}

/// Checks that a database is whole, was written by Agents Hub, and that this
/// version can read it. The error says what is wrong with it.
fn check_import(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = match conn.pragma_query_value(None, "user_version", |row| row.get(0)) {
        Ok(version) => version,
        Err(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ErrorCode::NotADatabase =>
        {
            bail!("it isn't a database")
        }
        Err(err) => return Err(err).context("it can't be read"),
    };
    // Agents Hub only ever counts up from 0, and a negative version would
    // count past every migration.
    if version <= 0 {
        bail!("it isn't an Agents Hub database");
    }
    if version > SCHEMA {
        bail!("it was written by a newer Agents Hub (schema {version}, this one knows {SCHEMA})");
    }
    let integrity: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .context("it is damaged")?;
    if integrity != "ok" {
        bail!("it is damaged: {integrity}");
    }
    if shape(conn)? != expected_shape(version)? {
        bail!("it isn't an Agents Hub database: its tables aren't the ones Agents Hub writes");
    }
    if conn.prepare("PRAGMA foreign_key_check")?.exists([])? {
        bail!("it is damaged: some of its rows refer to rows that aren't there");
    }
    let mut agents = conn.prepare("SELECT agent FROM chats UNION SELECT agent FROM sessions")?;
    for label in agents.query_map([], |row| row.get::<_, String>(0))? {
        let label = label?;
        if Agent::from_label(&label).is_none() {
            bail!("it has chats with an agent this Agents Hub doesn't know: {label}");
        }
    }
    Ok(())
}

/// The tables, columns and indexes of a database, which one Agents Hub wrote
/// has exactly as the migrations up to its version made them.
fn shape(conn: &Connection) -> Result<Vec<String>> {
    let objects = conn
        .prepare(
            "SELECT type, name, tbl_name FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    let mut columns =
        conn.prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid")?;
    let mut shape = Vec::new();
    for (kind, name, table) in objects {
        shape.push(format!("{kind} {name} on {table}"));
        if kind == "table" {
            for column in columns.query_map([&name], |row| {
                Ok(format!(
                    "{name}.{} {} not null: {}, key: {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, i64>(3)?
                ))
            })? {
                shape.push(column?);
            }
        }
    }
    Ok(shape)
}

/// The shape of a database Agents Hub wrote at `version`.
fn expected_shape(version: i64) -> Result<Vec<String>> {
    let conn = Connection::open_in_memory()?;
    for migration in &MIGRATIONS[..version as usize] {
        conn.execute_batch(migration)?;
    }
    shape(&conn)
}

/// Puts a confirmed import in place of the database at `path`, once the
/// database is backed up beside it. Also deletes the imports the user never
/// confirmed. Nothing may have the database open.
fn take_import(path: &Path) -> Option<Imported> {
    for copy in pending_imports(path) {
        remove_database(&copy);
    }
    let staged = beside(path, STAGED_IMPORT);
    if !staged.exists() {
        return None;
    }
    let replaced = replace_database(path, &staged);
    if replaced.is_err() {
        remove_database(&staged);
    }
    Some(match replaced {
        Ok(backup) => Imported::Replaced(backup),
        Err(err) => Imported::Failed(err),
    })
}

fn replace_database(path: &Path, staged: &Path) -> anyhow::Result<Option<PathBuf>> {
    let backup = if path.exists() {
        let stamp = jiff::Zoned::now().strftime("%Y-%m-%d-%H%M%S");
        let backup = beside(path, &format!("before-import-{stamp}.bak"));
        let conn = Connection::open(path).context("can't open the database it replaces")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // Reads through the write-ahead log, so nothing committed is lost.
        vacuum_into(&conn, &backup).context("can't back up the database it replaces")?;
        drop(conn);
        Some(backup)
    } else {
        None
    };
    // SQLite would apply the old database's write-ahead log to the new one.
    for suffix in ["wal", "shm", "journal"] {
        let file = beside_with(path, "-", suffix);
        match fs::remove_file(&file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("can't remove {}", file.display()));
            }
            _ => {}
        }
    }
    fs::rename(staged, path).context("can't move it in place of the database")?;
    restrict(path, 0o600);
    Ok(backup)
}

/// The copies of picked databases beside the database at `path`, whether or
/// not they were confirmed.
fn pending_imports(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let mut prefix = name.to_owned();
    prefix.push(format!(".{PENDING_IMPORT}-"));
    let prefix = prefix.as_encoded_bytes();
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        // What SQLite keeps beside each copy goes with it.
        .filter(|name| {
            let name = name.as_encoded_bytes();
            name.strip_prefix(prefix)
                .is_some_and(|id| !id.is_empty() && !id.contains(&b'-'))
        })
        .map(|name| dir.join(name))
        .collect()
}

/// `path` with `.suffix` after its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    beside_with(path, ".", suffix)
}

fn beside_with(path: &Path, separator: &str, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(separator);
    name.push(suffix);
    PathBuf::from(name)
}

/// Deletes a database and what SQLite keeps beside it, where there is any.
fn remove_database(path: &Path) {
    fs::remove_file(path).ok();
    for suffix in ["wal", "shm", "journal"] {
        fs::remove_file(beside_with(path, "-", suffix)).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(db: &Db) -> i64 {
        let project = db.add_project(Path::new("/home/me/project")).unwrap();
        db.add_chat(project, Agent::Claude, None).unwrap()
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

    /// A chat's sessions, and why it left each one.
    fn order(db: &Db, chat: i64) -> Vec<(String, Option<String>)> {
        db.sessions(chat)
            .unwrap()
            .into_iter()
            .map(|session| (session.agent_id, session.ended))
            .collect()
    }

    /// The texts of the items a session shows.
    fn texts(db: &Db, session: i64) -> Vec<String> {
        db.items(session, 100)
            .unwrap()
            .into_iter()
            .map(|item| item.payload["text"].as_str().unwrap().to_string())
            .collect()
    }

    fn item(source_id: &str, parent_id: Option<&str>, text: &str) -> transcript::Item {
        transcript::Item {
            source_id: Some(source_id.into()),
            parent_id: parent_id.map(String::from),
            kind: Kind::User,
            at: None,
            payload: json!({ "text": text }),
        }
    }

    /// Adds a line that kept `item` to a read, in the order it was read.
    fn push(read: &mut transcript::Read, item: transcript::Item) {
        let id = item.source_id.clone().unwrap();
        read.parents.insert(id.clone(), item.parent_id.clone());
        read.lines.push(id);
        read.items.push(item);
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds the items of one read, each line one item, chained.
    fn chain(first: usize, last: usize) -> transcript::Read {
        let mut read = transcript::Read::default();
        let mut parent: Option<String> = None;
        for id in first..=last {
            let id = id.to_string();
            push(&mut read, item(&id, parent.as_deref(), &id));
            parent = Some(id);
        }
        read
    }

    fn message(id: &str, kind: Kind, text: &str) -> transcript::Item {
        transcript::Item {
            source_id: Some(id.into()),
            parent_id: None,
            kind,
            at: None,
            payload: json!({ "text": text }),
        }
    }

    fn save(db: &Db, session: i64, items: Vec<transcript::Item>) {
        let mut read = transcript::Read::default();
        for item in items {
            push(&mut read, item);
        }
        db.save_read_state(session, &read).unwrap();
    }

    fn found(db: &Db, input: &str) -> Vec<String> {
        db.search(input)
            .unwrap()
            .into_iter()
            .map(|hit| hit.snippet)
            .collect()
    }

    fn check_index(db: &Db) {
        db.write()
            .execute(
                "INSERT INTO items_fts (items_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .unwrap();
    }

    #[test]
    fn keeps_a_second_copy_off_the_same_database_only() {
        let base = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        let first = lock_dir(&base.join("one")).unwrap();
        let err = lock_dir(&base.join("one")).unwrap_err();
        assert_eq!(err.to_string(), "Agents Hub is already running.");
        let _other = lock_dir(&base.join("two")).unwrap();
        drop(first);
        lock_dir(&base.join("one")).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn search_finds_words_and_the_start_of_the_last_one() {
        let db = Db::open_in_memory().unwrap();
        let chat_id = chat(&db);
        let session = record(&db, chat_id, "a", "startup").session_id;
        save(
            &db,
            session,
            vec![message(
                "1",
                Kind::User,
                "fix the\nopen_chat Résumé migration",
            )],
        );

        let hit = db.search("migr").unwrap().pop().unwrap();
        assert_eq!(hit.chat_id, chat_id);
        assert!(hit.prompt);
        assert_eq!(hit.snippet, "fix the open_chat Résumé migration");
        assert_eq!(found(&db, "open_chat").len(), 1);
        assert_eq!(found(&db, "chat open").len(), 1);
        assert_eq!(found(&db, "resume").len(), 1);
        assert!(found(&db, "igration").is_empty());
        assert!(found(&db, "fix absent").is_empty());
    }

    #[test]
    fn search_reads_nothing_typed_as_syntax() {
        let db = Db::open_in_memory().unwrap();
        let session = record(&db, chat(&db), "a", "startup").session_id;
        save(&db, session, vec![message("1", Kind::User, "foo bar")]);

        assert_eq!(found(&db, "foo-bar").len(), 1);
        for input in [
            "a:b",
            "AND",
            "\"unbalanced",
            "(x",
            "NEAR(",
            "-",
            "*",
            "\"\"",
        ] {
            assert!(db.search(input).is_ok(), "{input}");
        }
    }

    #[test]
    fn search_leaves_out_tools_and_reasoning() {
        let db = Db::open_in_memory().unwrap();
        let session = record(&db, chat(&db), "a", "startup").session_id;
        save(
            &db,
            session,
            vec![
                message("1", Kind::Reasoning, "needle"),
                message("2", Kind::Tool, "needle"),
                message("3", Kind::Notice, "needle"),
            ],
        );
        assert!(found(&db, "needle").is_empty());
    }

    #[test]
    fn search_puts_prompts_first_and_limits_each_chat() {
        let db = Db::open_in_memory().unwrap();
        let (first, second) = (chat(&db), chat(&db));
        let session = record(&db, first, "a", "startup").session_id;
        save(
            &db,
            session,
            vec![
                message("1", Kind::Assistant, "needle reply"),
                message("2", Kind::Assistant, "needle reply"),
                message("3", Kind::User, "needle prompt"),
                message("4", Kind::User, "needle prompt"),
                message("5", Kind::User, "needle prompt"),
                message("6", Kind::User, "needle prompt"),
            ],
        );
        let session = record(&db, second, "b", "startup").session_id;
        save(
            &db,
            session,
            vec![message("1", Kind::Assistant, "needle reply")],
        );

        let hits = db.search("needle").unwrap();
        let of = |chat| hits.iter().filter(|hit| hit.chat_id == chat).count();
        assert_eq!((of(first), of(second)), (3, 1));
        let prompts: Vec<_> = hits.iter().map(|hit| hit.prompt).collect();
        assert_eq!(prompts, [true, true, true, false]);
    }

    #[test]
    fn search_stops_at_the_limit() {
        let db = Db::open_in_memory().unwrap();
        for id in 0..SEARCH_HITS + 5 {
            let id = id.to_string();
            let session = record(&db, chat(&db), &id, "startup").session_id;
            save(&db, session, vec![message("1", Kind::User, "needle")]);
        }
        assert_eq!(found(&db, "needle").len(), SEARCH_HITS as usize);
    }

    #[test]
    fn the_index_follows_rereads_and_deletions() {
        let db = Db::open_in_memory().unwrap();
        let chat_id = chat(&db);
        let session = record(&db, chat_id, "a", "startup").session_id;
        save(
            &db,
            session,
            vec![
                message("1", Kind::User, "before"),
                message("2", Kind::Tool, "tool"),
            ],
        );
        check_index(&db);

        let mut read = transcript::Read::default();
        read.reset = true;
        push(&mut read, message("3", Kind::User, "after"));
        db.save_read_state(session, &read).unwrap();
        assert!(found(&db, "before").is_empty());
        assert_eq!(found(&db, "after").len(), 1);
        check_index(&db);

        db.delete_chat(chat_id).unwrap();
        assert!(found(&db, "after").is_empty());
        check_index(&db);
    }

    #[test]
    fn migrating_indexes_what_was_already_read() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        for migration in &MIGRATIONS[..6] {
            conn.execute_batch(migration).unwrap();
        }
        conn.execute_batch(
            r#"INSERT INTO projects (id, path, name, created_at) VALUES (1, '/p', 'p', 0);
               INSERT INTO chats (id, project_id, agent, created_at, updated_at)
               VALUES (1, 1, 'claude', 0, 0);
               INSERT INTO sessions (id, agent, agent_id, transcript_path, started_at)
               VALUES (1, 'claude', 'a', '/t', 0);
               INSERT INTO chat_sessions (chat_id, session_id, position, first_position)
               VALUES (1, 1, 0, 0);
               INSERT INTO items (id, session_id, kind, payload) VALUES
                   (1, 1, 'user', '{"text":"needle"}'),
                   (2, 1, 'tool', '{"name":"grep","input":"needle"}');"#,
        )
        .unwrap();
        migrate(&mut conn, 6).unwrap();

        let db = Db {
            write: Arc::new(Mutex::new(conn)),
            read: None,
            path: None,
            dir: None,
        };
        assert_eq!(found(&db, "needle"), ["needle"]);
        check_index(&db);
    }

    #[test]
    fn a_database_with_the_index_can_be_exported_and_imported() {
        let dir = temp_dir();
        let db = Db::open(&dir.join("agentshub.db")).unwrap();
        let session = record(&db, chat(&db), "a", "startup").session_id;
        save(&db, session, vec![message("1", Kind::User, "needle")]);

        let exports = dir.join("exports");
        fs::create_dir(&exports).unwrap();
        let export = exports.join("export.db");
        db.export(&export).unwrap();
        let conn = Connection::open_with_flags(&export, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        check_import(&conn).unwrap();
        drop(conn);

        let (_, projects) = prepare_import_beside(&dir.join("other").join("db"), &export).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(found(&Db::open(&export).unwrap(), "needle"), ["needle"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_the_database_folder_to_its_user() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let data = dir.join("agentshub");
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();

        Db::open(&data.join("agentshub.db")).unwrap();
        let mode = fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_can_hang_off_a_line_the_read_before_dropped() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;

        // A kept line, then a line that keeps no item, which the read ends on.
        let mut read = transcript::Read::default();
        read.offset = 20;
        push(&mut read, item("1", None, "first"));
        read.parents.insert("2".into(), Some("1".into()));
        read.lines.push("2".into());
        assert!(db.save_read_state(session, &read).unwrap());

        // The next message's parent is that dropped line.
        let mut read = transcript::Read::default();
        read.offset = 40;
        push(&mut read, item("3", Some("2"), "second"));
        assert!(db.save_read_state(session, &read).unwrap());

        assert_eq!(texts(&db, session), ["first", "second"]);
    }

    #[test]
    fn rereads_and_deletes_a_session_with_a_long_branch() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;
        // Each item hangs off the one before it, deeper than SQLite lets a
        // cascade go.
        let mut read = chain(1, 1500);
        read.offset = 10;
        db.save_read_state(session, &read).unwrap();

        let mut reread = chain(1, 1500);
        reread.reset = true;
        reread.offset = 10;
        assert!(db.save_read_state(session, &reread).unwrap());
        assert_eq!(db.items(session, 2000).unwrap().len(), 1500);

        db.delete_chat(chat).unwrap();
        assert!(db.transcript(session).unwrap().is_none());
    }

    #[test]
    fn items_page_from_the_end_of_the_branch() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;
        let mut read = chain(1, 5);
        read.offset = 10;
        db.save_read_state(session, &read).unwrap();

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
        let elsewhere = temp_dir();
        let copy = elsewhere.join("copy.db");

        db.export(&copy).unwrap();
        let exported = Db::open(&copy).unwrap();
        assert_eq!(exported.projects().unwrap().len(), 1);
        assert_eq!(exported.sessions(chat).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(elsewhere).unwrap();
    }

    #[test]
    fn does_not_export_over_the_database() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let db = Db::open(&path).unwrap();
        let chat = chat(&db);

        assert!(db.export(&path).is_err());
        assert!(db.export(&dir.join("copy.db")).is_err());
        // The database is still there, and still takes writes.
        record(&db, chat, "a", "startup");
        drop(db);
        assert_eq!(Db::open(&path).unwrap().sessions(chat).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_keep_their_last_value() {
        use rusqlite::types::Value;

        let db = Db::open_in_memory().unwrap();
        assert!(db.settings().unwrap().is_empty());
        db.set_flag("flag", true).unwrap();
        db.set_flag("flag", false).unwrap();
        db.set_text("text", "light").unwrap();
        db.set_text("text", "dark").unwrap();
        // The column's affinity stores this one as an integer.
        db.set_number("whole", 450.).unwrap();
        db.set_number("fraction", 312.5).unwrap();
        let saved = db.settings().unwrap();
        assert_eq!(saved["flag"], Value::Integer(0));
        assert_eq!(saved["text"], Value::Text("dark".into()));
        assert_eq!(saved["whole"], Value::Integer(450));
        assert_eq!(saved["fraction"], Value::Real(312.5));
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
    fn a_chat_in_a_worktree_runs_there() {
        let db = Db::open_in_memory().unwrap();
        let project = db.add_project(Path::new("/home/me/project")).unwrap();
        let root = PathBuf::from("/home/me/.local/share/agentshub/worktrees/project/1a2b3c4d");
        let worktree = Worktree {
            folder: root.join("app"),
            root,
        };
        let plain = db.add_chat(project, Agent::Claude, None).unwrap();
        let isolated = db.add_chat(project, Agent::Pi, Some(&worktree)).unwrap();
        assert_eq!(
            db.chat(plain).unwrap(),
            Some((Agent::Claude, PathBuf::from("/home/me/project")))
        );
        assert_eq!(
            db.chat(isolated).unwrap(),
            Some((Agent::Pi, worktree.folder.clone()))
        );
        let chats = &db.projects().unwrap()[0].chats;
        let listed = |id| chats.iter().find(|chat| chat.id == id).unwrap();
        assert_eq!(listed(plain).worktree, None);
        assert_eq!(listed(isolated).worktree.as_ref(), Some(&worktree));
    }

    #[test]
    fn a_removed_project_has_no_path_even_when_its_id_is_reused() {
        let db = Db::open_in_memory().unwrap();
        let old = db.add_project(Path::new("/home/me/old")).unwrap();
        assert_eq!(
            db.project_path(old).unwrap(),
            Some(PathBuf::from("/home/me/old"))
        );
        db.remove_project(old).unwrap();
        assert_eq!(db.project_path(old).unwrap(), None);
        let new = db.add_project(Path::new("/home/me/new")).unwrap();
        // Whether or not SQLite reuses the id, it doesn't name the old folder.
        assert_ne!(
            db.project_path(new).unwrap(),
            Some(PathBuf::from("/home/me/old"))
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
        let c = record(&db, chat, "c", "new");
        // `/resume` goes back to the first one.
        assert_eq!(record(&db, chat, "a", "resume").left, Some(c.session_id));
        assert_eq!(
            order(&db, chat),
            [
                ("b".into(), Some("new".into())),
                ("c".into(), Some("resume".into())),
                // Re-entering a session forgets why the chat left it.
                ("a".into(), None),
            ]
        );
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
    fn a_replayed_line_does_not_move_the_leaf_back() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;

        // The newest line is the second message, and the last line is a
        // replay of the first one.
        let mut read = transcript::Read::default();
        read.offset = 30;
        push(&mut read, item("1", None, "first"));
        push(&mut read, item("2", Some("1"), "second"));
        push(&mut read, item("1", None, "first"));
        assert!(db.save_read_state(session, &read).unwrap());

        assert_eq!(texts(&db, session), ["first", "second"]);
    }

    #[test]
    fn a_link_that_changes_the_branch_says_the_history_changed() {
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = record(&db, chat, "a", "startup").session_id;

        let mut read = transcript::Read::default();
        read.offset = 10;
        push(&mut read, item("a", None, "first"));
        push(&mut read, item("b", Some("a"), "abandoned"));
        assert!(db.save_read_state(session, &read).unwrap());

        // Pi's `/tree` appends only a branch summary, which moves the leaf
        // back and hides the abandoned branch.
        let mut read = transcript::Read::default();
        read.offset = 20;
        read.parents.insert("s".into(), Some("a".into()));
        read.lines.push("s".into());
        assert!(db.save_read_state(session, &read).unwrap());
        assert_eq!(texts(&db, session), ["first"]);
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
        assert_eq!(order(&db, second), [("shared".into(), None)]);

        db.remove_project(db.projects().unwrap()[0].id).unwrap();
        assert!(db.transcript(shared).unwrap().is_none());
        assert!(db.projects().unwrap().is_empty());
    }

    /// Saves a first prompt for a session that has not been read.
    fn save_prompt_for_title_test(db: &Db, session: i64, prompt: &str) {
        let mut read = transcript::Read::default();
        read.prompt = Some(prompt.into());
        read.prompted = true;
        db.save_read_state(session, &read).unwrap();
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
        push(&mut stale, item("1", None, "deleted"));
        assert!(!db.save_read_state(old, &stale).unwrap());
        assert!(db.items(new, 100).unwrap().is_empty());
        assert_eq!(db.transcript(new).unwrap().unwrap().offset, 0);
        assert!(db.sessions(second).unwrap().iter().all(|s| !s.prompted));
    }

    #[test]
    fn a_prompt_without_a_time_still_counts_as_prompted() {
        let db = Db::open_in_memory().unwrap();
        let session = record(&db, chat(&db), "a", "startup").session_id;
        assert_eq!(db.last_prompt(session).unwrap(), (false, None));

        let mut read = transcript::Read::default();
        read.prompted = true;
        push(&mut read, item("1", None, "hello"));
        db.save_read_state(session, &read).unwrap();
        assert_eq!(db.last_prompt(session).unwrap(), (true, None));
    }

    /// The ids of the first project's chats, in order.
    fn chat_order(db: &Db) -> Vec<i64> {
        db.projects().unwrap()[0]
            .chats
            .iter()
            .map(|chat| chat.id)
            .collect()
    }

    #[test]
    fn pins_and_moves_chats_within_their_group() {
        let db = Db::open_in_memory().unwrap();
        let [a, b, c, d] = [(); 4].map(|_| chat(&db));
        assert_eq!(chat_order(&db), [d, c, b, a]);

        // A chat takes the place of the one it is moved to, up or down.
        db.move_chat(d, b).unwrap();
        assert_eq!(chat_order(&db), [c, b, d, a]);
        db.move_chat(a, c).unwrap();
        assert_eq!(chat_order(&db), [a, c, b, d]);

        db.set_pinned(b, true).unwrap();
        db.set_pinned(d, true).unwrap();
        assert_eq!(chat_order(&db), [d, b, a, c]);
        db.move_chat(b, d).unwrap();
        assert_eq!(chat_order(&db), [b, d, a, c]);
        // Pinned and unpinned chats don't move into each other's group.
        db.move_chat(a, d).unwrap();
        assert_eq!(chat_order(&db), [b, d, a, c]);

        let e = chat(&db);
        assert_eq!(chat_order(&db), [b, d, e, a, c]);
        db.set_pinned(b, false).unwrap();
        assert_eq!(chat_order(&db), [d, b, e, a, c]);
    }

    #[test]
    fn a_chat_keeps_the_title_it_was_given() {
        let db = Db::open_in_memory().unwrap();
        let id = chat(&db);
        let session = record(&db, id, "s", "startup").session_id;
        save_prompt_for_title_test(&db, session, "from the session");
        let title = |db: &Db| db.projects().unwrap()[0].chats[0].title.clone();

        db.rename_chat(id, "  mine ").unwrap();
        assert_eq!(title(&db).as_deref(), Some("mine"));
        db.rename_chat(id, " ").unwrap();
        assert_eq!(title(&db).as_deref(), Some("from the session"));
    }

    #[test]
    fn migrating_keeps_the_order_chats_were_shown_in() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 1;
             INSERT INTO projects VALUES (1, '/p', 'p', 0), (2, '/q', 'q', 0);
             INSERT INTO chats (id, project_id, agent, created_at, updated_at) VALUES
                 (1, 1, 'claude', 0, 5), (2, 1, 'claude', 0, 9), (3, 1, 'claude', 0, 5),
                 (4, 2, 'claude', 0, 1);",
        )
        .unwrap();
        let db = Db::init(conn, None).unwrap();
        let projects = db.projects().unwrap();
        let order = |project: &Project| project.chats.iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(order(&projects[0]), [4]);
        assert_eq!(order(&projects[1]), [2, 3, 1]);
    }

    #[test]
    fn migrating_keeps_the_order_projects_were_shown_in() {
        let conn = Connection::open_in_memory().unwrap();
        for migration in &MIGRATIONS[..3] {
            conn.execute_batch(migration).unwrap();
        }
        conn.execute_batch(
            "PRAGMA user_version = 3;
             INSERT INTO projects VALUES (1, '/p', 'p', 5), (2, '/q', 'q', 9), (3, '/r', 'r', 5);",
        )
        .unwrap();
        let db = Db::init(conn, None).unwrap();
        assert_eq!(project_order(&db), [2, 3, 1]);
    }

    /// The ids of the projects, in order.
    fn project_order(db: &Db) -> Vec<i64> {
        db.projects()
            .unwrap()
            .iter()
            .map(|project| project.id)
            .collect()
    }

    #[test]
    fn adds_projects_at_the_top_and_moves_them() {
        let db = Db::open_in_memory().unwrap();
        let [a, b, c, d] = ["a", "b", "c", "d"]
            .map(|name| db.add_project(&Path::new("/home/me").join(name)).unwrap());
        assert_eq!(project_order(&db), [d, c, b, a]);

        // A project takes the place of the one it is moved to, up or down.
        db.move_project(d, b).unwrap();
        assert_eq!(project_order(&db), [c, b, d, a]);
        db.move_project(a, c).unwrap();
        assert_eq!(project_order(&db), [a, c, b, d]);

        // Adding a folder again leaves its project where it is.
        assert_eq!(db.add_project(Path::new("/home/me/b")).unwrap(), b);
        assert_eq!(project_order(&db), [a, c, b, d]);

        let e = db.add_project(Path::new("/home/me/e")).unwrap();
        assert_eq!(project_order(&db), [e, a, c, b, d]);

        // Adding an archived folder again brings its project back on top.
        db.set_project_archived(b, true).unwrap();
        assert_eq!(db.add_project(Path::new("/home/me/b")).unwrap(), b);
        assert_eq!(project_order(&db), [b, e, a, c, d]);
        assert!(db.projects().unwrap()[0].archived_at.is_none());
    }

    /// The ids of the first project's archived chats.
    fn archived(db: &Db) -> Vec<i64> {
        db.projects().unwrap()[0]
            .chats
            .iter()
            .filter(|chat| chat.archived_at.is_some())
            .map(|chat| chat.id)
            .collect()
    }

    #[test]
    fn an_undone_archive_keeps_a_chat_in_its_place() {
        let db = Db::open_in_memory().unwrap();
        let [a, b, c] = [(); 3].map(|_| chat(&db));
        db.set_chat_archived(b, true).unwrap();
        assert_eq!(archived(&db), [b]);
        db.set_chat_archived(b, false).unwrap();
        assert_eq!(archived(&db), [] as [i64; 0]);
        assert_eq!(chat_order(&db), [c, b, a]);
    }

    #[test]
    fn restoring_a_chat_brings_it_and_its_project_back_on_top() {
        let db = Db::open_in_memory().unwrap();
        let [a, b, c] = [(); 3].map(|_| chat(&db));
        let project = db.projects().unwrap()[0].id;
        let other = db.add_project(Path::new("/home/me/other")).unwrap();
        db.set_chat_archived(a, true).unwrap();
        db.set_project_archived(project, true).unwrap();

        assert!(db.restore_chat(a).unwrap());
        assert_eq!(project_order(&db), [project, other]);
        assert_eq!(archived(&db), [] as [i64; 0]);
        assert_eq!(chat_order(&db), [a, c, b]);
        assert!(db.projects().unwrap()[0].archived_at.is_none());
        // Nothing was archived anymore.
        assert!(!db.restore_chat(a).unwrap());

        db.set_project_archived(other, true).unwrap();
        assert!(db.restore_project(other).unwrap());
        assert_eq!(project_order(&db), [other, project]);
        assert!(!db.restore_project(other).unwrap());
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

    #[test]
    fn a_newer_parser_reads_a_transcript_again() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let line = json!({"type": "user", "uuid": "1", "message": {"content": "first"}});
        fs::write(&path, format!("{line}\n")).unwrap();
        let db = Db::open_in_memory().unwrap();
        let chat = chat(&db);
        let session = db
            .record_session(chat, Agent::Claude, "abc", &path, Some("startup"))
            .unwrap()
            .session_id;
        transcript::read(&db, session).unwrap();

        // An older parser read the whole file, but kept nothing of it.
        db.write()
            .execute_batch("DELETE FROM items; UPDATE sessions SET parser_version = 0;")
            .unwrap();
        assert!(transcript::read(&db, session).unwrap());
        assert_eq!(texts(&db, session), ["first"]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A database at `path` with one project, named after its folder, and
    /// `chats` chats in it.
    fn database_with(path: &Path, folder: &str, chats: usize) -> Db {
        let db = Db::open(path).unwrap();
        let project = db.add_project(Path::new(folder)).unwrap();
        for _ in 0..chats {
            db.add_chat(project, Agent::Claude, None).unwrap();
        }
        db
    }

    fn project_names(db: &Db) -> Vec<String> {
        db.projects()
            .unwrap()
            .into_iter()
            .map(|project| project.name)
            .collect()
    }

    #[test]
    fn imports_a_database_in_place_of_the_one_it_backs_up() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let current = database_with(&path, "/home/me/current", 1);
        let elsewhere = temp_dir();
        let source = elsewhere.join("other.db");
        database_with(&source, "/home/me/imported", 2);

        let (pending, projects) = prepare_import_beside(&path, &source).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "imported");
        assert_eq!(projects[0].chats.len(), 2);
        pending.confirm_beside(&path).unwrap();
        // Agents Hub restarts, so nothing has the database open.
        drop(current);

        let Some(Imported::Replaced(Some(backup))) = take_import(&path) else {
            panic!("the import wasn't put in place");
        };
        assert_eq!(project_names(&Db::open(&path).unwrap()), ["imported"]);
        assert_eq!(project_names(&Db::open(&backup).unwrap()), ["current"]);
        // It is done once.
        assert!(take_import(&path).is_none());
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(elsewhere).unwrap();
    }

    #[test]
    fn an_import_the_user_did_not_confirm_is_dropped() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        drop(database_with(&path, "/home/me/current", 1));
        let source = dir.join("other.db");
        drop(database_with(&source, "/home/me/imported", 1));

        let (pending, _) = prepare_import_beside(&path, &source).unwrap();
        assert!(pending.copy.exists());
        assert!(take_import(&path).is_none());
        assert!(!pending.copy.exists());
        assert_eq!(project_names(&Db::open(&path).unwrap()), ["current"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn overlapping_imports_keep_their_own_copies() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let current = database_with(&path, "/home/me/current", 1);
        let first = dir.join("first.db");
        drop(database_with(&first, "/home/me/first", 1));
        let second = dir.join("second.db");
        drop(database_with(&second, "/home/me/second", 1));

        let (first, _) = prepare_import_beside(&path, &first).unwrap();
        let (second, _) = prepare_import_beside(&path, &second).unwrap();
        // Turning the second down leaves the first to confirm.
        second.discard();
        first.confirm_beside(&path).unwrap();
        drop(current);

        assert!(matches!(take_import(&path), Some(Imported::Replaced(_))));
        assert_eq!(project_names(&Db::open(&path).unwrap()), ["first"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn brings_an_older_database_up_to_date_on_import() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let source = dir.join("old.db");
        {
            let conn = Connection::open(&source).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1i64).unwrap();
            conn.execute(
                "INSERT INTO projects (path, name, created_at) VALUES ('/home/me/old', 'old', 0)",
                [],
            )
            .unwrap();
        }

        let (pending, projects) = prepare_import_beside(&path, &source).unwrap();
        assert_eq!(projects[0].name, "old");
        let pending = Connection::open(&pending.copy).unwrap();
        let version: i64 = pending
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_to_import_what_agentshub_did_not_write() {
        let dir = temp_dir();
        let path = dir.join("agentshub.db");
        let refusal = |source: &Path| {
            let err = prepare_import_beside(&path, source)
                .err()
                .expect("it was imported");
            assert!(pending_imports(&path).is_empty());
            format!("{err:#}")
        };

        let text = dir.join("notes.txt");
        fs::write(
            &text,
            "not a database, but long enough to look like a header",
        )
        .unwrap();
        assert!(refusal(&text).contains("isn't a database"));

        let empty = dir.join("empty.db");
        Connection::open(&empty)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        assert!(refusal(&empty).contains("isn't an Agents Hub database"));

        let other = dir.join("other.db");
        {
            let conn = Connection::open(&other).unwrap();
            conn.execute_batch("CREATE TABLE projects (id INTEGER PRIMARY KEY)")
                .unwrap();
            conn.pragma_update(None, "user_version", SCHEMA).unwrap();
        }
        assert!(refusal(&other).contains("tables aren't"));

        let negative = dir.join("negative.db");
        {
            let conn = Connection::open(&negative).unwrap();
            conn.execute_batch("CREATE TABLE t (x)").unwrap();
            conn.pragma_update(None, "user_version", -1i64).unwrap();
        }
        assert!(refusal(&negative).contains("isn't an Agents Hub database"));

        let newer = dir.join("newer.db");
        Connection::open(&newer)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA + 1)
            .unwrap();
        assert!(refusal(&newer).contains("newer"));

        let unknown = dir.join("unknown.db");
        {
            let db = database_with(&unknown, "/home/me/project", 1);
            db.write()
                .execute("UPDATE chats SET agent = 'someday'", [])
                .unwrap();
        }
        assert!(refusal(&unknown).contains("someday"));

        let dangling = dir.join("dangling.db");
        {
            let db = database_with(&dangling, "/home/me/project", 1);
            let conn = db.write();
            conn.execute_batch("PRAGMA foreign_keys = OFF; DELETE FROM projects;")
                .unwrap();
        }
        assert!(refusal(&dangling).contains("refer to rows"));

        drop(database_with(&path, "/home/me/current", 1));
        assert!(refusal(&path).contains("is using"));
        fs::remove_dir_all(dir).unwrap();
    }
}
