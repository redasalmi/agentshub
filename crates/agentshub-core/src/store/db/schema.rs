//! The database schema, its migrations and the copies taken before them.

use super::*;

/// Each migration takes the database from the version before it, as counted
/// by `PRAGMA user_version`, to the next.
pub(super) const MIGRATIONS: [&str; 7] = [
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
#[expect(
    clippy::cast_possible_wrap,
    reason = "a const can't use try_from, and there are far fewer migrations than i64::MAX"
)]
pub(super) const SCHEMA: i64 = MIGRATIONS.len() as i64;

/// Takes a database from `version` to the one this build writes.
pub(super) fn migrate(conn: &mut Connection, version: i64) -> Result<()> {
    for (done, migration) in MIGRATIONS
        .iter()
        .enumerate()
        .skip(usize::try_from(version).unwrap_or(usize::MAX))
    {
        let tx = conn.transaction()?;
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", done + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Copies a database about to be migrated, since what it holds can't be
/// recovered anywhere else.
pub(super) fn backup(conn: &Connection, path: &Path, version: i64) -> anyhow::Result<()> {
    let backup = PathBuf::from(format!("{}.v{version}.bak", path.display()));
    vacuum_into(conn, &backup).context("can't back up the database")
}

/// Writes a consistent copy of the database over `path` with SQLite's own
/// `VACUUM INTO`, which leaves the original alone.
pub(super) fn vacuum_into(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    fs::remove_file(path).ok();
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
        .with_context(|| format!("can't write {}", path.display()))?;
    restrict(path, 0o600);
    Ok(())
}
