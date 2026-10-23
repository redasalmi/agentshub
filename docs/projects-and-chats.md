# Projects and chats

Status: implemented (schema 5).

## Goal

agentshub stops listing the sessions the CLIs keep on disk and keeps its own
projects and chats in a SQLite database.

- A **project** is a folder the user adds with a folder picker.
- A **chat** belongs to a project and runs one agent (Claude Code, Codex or
  Pi) in the terminal, as today.
- A chat can go through several **agent sessions**: `/clear`, `/new`, `/resume`
  and `/fork` switch the session inside the running CLI.
- Reopening a chat shows its earlier sessions as a collapsible, read-only list
  and resumes its last session in the terminal.

## Background

No CLI can load more than one session: each resumes exactly one
(`claude --resume <id>`, `codex resume <id>`, `pi --session <path>`). So
agentshub has to keep the order of a chat's sessions itself, and show the
earlier ones itself.

The CLIs' own files can't be the history either:

- Claude Code deletes transcripts 30 days after their last activity
  (`cleanupPeriodDays`).
- The files are large: about 586 MB of Claude transcripts for one month, 1 GB of
  Codex rollouts for three weeks, 322 MB of Pi sessions for six weeks. Prompts,
  replies and tool calls are a few percent of that. The rest is duplicated tool
  results, images, injected context and request records.
- A cloud version would run the CLIs on machines whose disks don't last.

t3code and bb solve this the same way: they keep their own trimmed copy of the
conversation in their database, show history from it, and use the agent's own
file only to resume. Paseo reads the agent's files every time, and loses earlier
sessions after a restart. This design follows t3code and bb.

### What the trimmed copy keeps

| Kept | Dropped |
|---|---|
| User prompts | Context the CLIs inject as user messages (the lists in `transcript.rs`) |
| Assistant replies | Claude's `toolUseResult` duplicates, `attachment`, `api-request*`, `file-history-snapshot`, `last-prompt` records |
| Reasoning text | Codex `event_msg`, `token_count`, `turn_context`, `world_state`, developer messages |
| Tool calls: name and input | Pi `model_change`, `thinking_level_change`, `custom` entries |
| Tool output, cut to a limit | Images (an `[image]` placeholder stays) |
| Compaction points | Token usage, per-line bookkeeping (cwd, version, git branch) |
| Timestamps and model | Subagent transcripts (Claude `isSidechain` lines and `subagents/` files) |

Reasoning is kept in full: it is a few percent of the files and the only record
of why an agent did something. Tool output is cut to its first and last 2 KB.

What this gives up: a session can't be resumed from the copy. Once Claude
deletes the last session of a chat, reopening the chat starts a new session
(see [Missing transcripts](#missing-transcripts)).

## Data

The database lives at `$XDG_DATA_HOME/agentshub/agentshub.db`
(`~/.local/share/agentshub/agentshub.db` by default). The folder is `0700` and
the file `0600`. It is **plaintext**: those permissions keep other local users
out, not backups, sync folders or an unlocked machine, and paths such as a
project's folder are in it too. Encrypting the disk is the operating system's
job; agentshub does not hold a key.

It is opened with `journal_mode = WAL`, `synchronous = NORMAL`, a `busy_timeout`
and `foreign_keys = ON`, and migrated with `PRAGMA user_version`. A database
from a newer version is refused. Before an existing database is migrated, a
copy is written beside it as `agentshub.db.v<old schema>.bak`; a migration
rewrites conversations that can't be recovered anywhere else.

Writes go through one connection, which a transcript read holds for the length
of its transaction. Reads have a second, read-only connection, so that the
window doesn't wait on those writes. Crate: `rusqlite` with the `bundled`
feature.

Only one Agents Hub runs at a time: it holds `flock` on
`$XDG_RUNTIME_DIR/agentshub.lock` before opening the database. A second instance
shows that it is already running, with a Quit button, and nothing else.

```sql
CREATE TABLE projects (
    id         INTEGER PRIMARY KEY,
    path       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,          -- the folder name, shown in the sidebar
    created_at INTEGER NOT NULL
);

CREATE TABLE chats (
    id         INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects ON DELETE CASCADE,
    agent      TEXT NOT NULL,          -- claude | codex | pi
    title      TEXT,                   -- set by a rename, which doesn't exist yet
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL        -- orders the chats in the sidebar
);

-- One row per agent session, shared by the chats it appears in.
CREATE TABLE sessions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    agent           TEXT NOT NULL,
    agent_id        TEXT NOT NULL,     -- the CLI's session id
    transcript_path TEXT NOT NULL,
    custom_title    TEXT,              -- the title is the custom one,
    generated_title TEXT,              -- then the generated one,
    prompt          TEXT,              -- then the first prompt
    prompted        INTEGER NOT NULL DEFAULT 0,  -- it may hold only an image
    started_at      INTEGER NOT NULL,
    read_offset     INTEGER NOT NULL DEFAULT 0,  -- bytes read so far
    leaf_item_id    INTEGER REFERENCES items (id) ON DELETE SET NULL,
    missing         INTEGER NOT NULL DEFAULT 0,  -- the transcript is gone
    UNIQUE (agent, agent_id)
);

-- The sessions of a chat, in order. The last one is resumed.
CREATE TABLE chat_sessions (
    chat_id        INTEGER NOT NULL REFERENCES chats ON DELETE CASCADE,
    session_id     INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    position       INTEGER NOT NULL,
    first_position INTEGER,            -- where it first entered the chat
    reason         TEXT,               -- startup | clear | new | resume | fork
    ended_reason   TEXT,               -- why the chat left it, for the list above
    PRIMARY KEY (chat_id, session_id),
    UNIQUE (chat_id, position)
);

-- The trimmed copy of each session.
CREATE TABLE items (
    id             INTEGER PRIMARY KEY,
    session_id     INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    source_id      TEXT,               -- the line's id in the transcript, if any
    parent_item_id INTEGER REFERENCES items (id) ON DELETE CASCADE,
    kind           TEXT NOT NULL,      -- user | assistant | reasoning | tool | compaction | notice
    at             INTEGER,
    payload        TEXT NOT NULL,      -- JSON, depends on kind
    UNIQUE (session_id, source_id)
);
CREATE INDEX items_session ON items (session_id, id);
-- A tool's output is matched to its call by the call's id in the payload.
CREATE INDEX items_tool_call ON items (session_id, json_extract(payload, '$.id'))
    WHERE kind = 'tool';

-- Transcript lines that kept no item, each with the item it hangs off. A later
-- message often has one of these lines as its parent, so a read has to know
-- where they sit even though they aren't shown.
CREATE TABLE lines (
    session_id INTEGER NOT NULL REFERENCES sessions ON DELETE CASCADE,
    source_id  TEXT NOT NULL,
    item_id    INTEGER REFERENCES items (id) ON DELETE CASCADE,
    PRIMARY KEY (session_id, source_id)
) WITHOUT ROWID;

-- Ids that must not be reused when a row is deleted and another created.
CREATE TABLE sequences (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
```

The three titles are kept apart because each can change on its own in a later
read: pi clears a custom title with an empty one, which brings the generated
one back.

Sessions have their own table because `/resume` can bring a session into a
second chat. It is read once, and both chats show it.

The `sequences` table keeps a deleted chat's or session's id from being handed
to a new row: a transcript read that started before the deletion must not find
the new session instead.

Payloads by kind:

- `user`, `assistant`, `reasoning`: `{ "text": "..." }`
- `tool`: `{ "id": "...", "name": "...", "input": ..., "output": "...", "truncated": bool, "error": bool }`.
  A tool's output arrives on a later line than the call. It is matched by the
  call id (`tool_use_id` for Claude, `call_id` for Codex, `toolCallId` for Pi)
  and written onto the call's row.
- `compaction`: `{}`
- `notice`: `{ "text": "..." }`, for things agentshub says, like a missing
  transcript

### Backups

Two ways to keep a copy:

- before a migration, automatically, as `agentshub.db.v<old>.bak` beside the
  database;
- on demand, from the sidebar's export button, which asks where to write and
  runs SQLite's `VACUUM INTO`, so the copy is consistent even while a CLI
  writes.

The history is never pruned: sessions stay until their chat or project is
deleted, even when their transcript is long gone. The file plateaus near its
peak rather than growing without bound, because SQLite reuses the pages freed
by deletions. Export is the escape hatch, not a retention policy.

## Tracking a chat's sessions

`reports.rs` learns the session a CLI is in. The reports are append-only:

- Claude: the SessionStart hook appends its input
  (`cat >> "$AGENTSHUB_REPORT"`), which carries `session_id`,
  `transcript_path` and `source` (`startup`, `resume`, `clear`, `compact`,
  `fork`).
- Pi: the extension appends `{ transcript_path, reason, previous }` on
  `session_start`, where `reason` is `startup`, `reload`, `new`, `resume` or
  `fork`.
- Codex: unchanged. Its session is found from the files it keeps open
  (`sessions::codex_session_of`). The reason is unknown.

The report file is read from where the last read stopped, and its JSON values
are read one after the other (`serde_json::Deserializer::into_iter`), so
values that span several lines still parse. Garbage that can't be told apart
from what follows it is dropped, not retried.

For each session reported:

1. Find or insert its `sessions` row. A Codex rollout's id is in its
   `session_meta` line. Claude's is in the report, and Pi's is in the file's
   `session` header.
2. If it is already the chat's last session, nothing changes. This is how
   `/compact` (same Claude session) is ignored.
3. If the chat already has it at another position, as when `/resume` goes back
   to an earlier session, move it to the end. Each session shows once, ordered
   by when it was last used. `first_position` keeps where it first entered, and
   the chat is titled after the earliest of those.
4. Otherwise append it to the end.
5. Read the session the chat just left (see [Reading transcripts](#reading-transcripts)).

Sessions without a single prompt (a `/clear` right after starting) aren't
shown.

Recording a session, and reading one that the chat is using right now, moves
the chat to the top of the sidebar; reading history does not.

## Reading transcripts

Transcripts are read into `items` incrementally, from `read_offset` to the last
complete line, on a blocking thread of its own, in one transaction per read. A
line still being written is read next time. This is the same pattern
`SessionFile::read` used to use.

A session is read:

- when its row is expanded in the history, for the first time since the chat
  opened,
- when the chat switches away from it,
- when the chat's CLI exits,
- on workspace shutdown, after stopping the CLIs and draining their final
  session reports, before removing the report files,
- when the chat is opened, for its last session only, since that decides how
  the CLI starts,
- when the live session's agent finishes a turn, so the chat's title in the
  sidebar follows it.

Opening a chat does not read its earlier sessions. Their rows only need
metadata, and their items are read when the user asks to see them.

The terminal shows the live session, so there is no need to watch transcript
files.

Edge cases:

- A file shorter than `read_offset` was rewritten: delete the session's items
  and its lineage, and read it again from the start. Notices survive, since
  they are agentshub's own.
- A file that is gone: set `missing`, keep the items.
- Claude can append the same line again on resume. `UNIQUE (session_id,
  source_id)` keeps it once, and the leaf doesn't move back to it.

### Branches

Claude (`uuid`/`parentUuid`) and Pi (`id`/`parentId`) keep a tree in one file:
`/rewind` in Claude and `/tree` in Pi go back to an earlier message and continue
from there. Codex rollouts are linear.

Ancestry is resolved when a read is saved, not when the history is shown:

- Each item stores `parent_item_id`, the id of the nearest kept item above its
  line. A line that made several items gives its id to the last one; the others
  are `<id>:<n>` and chain to it.
- Lines that kept no item get one row in `lines` naming that same ancestor.
  They need it because the next message's parent is often one of them, such as
  Claude's `system/turn_duration`; the row is a single integer, not the uuid
  tree the display used to walk.
- `sessions.leaf_item_id` is the item the branch ended on. It can come from a
  line that kept nothing, such as a branch summary: the summary changes the
  branch even though it adds no item.

Showing a session walks `parent_item_id` back from the leaf in one recursive
query, so only the branch the session ended on shows. A Claude compaction
starts a new root and points back through `logicalParentUuid`; a Pi branch
summary moves the leaf back.

### Per agent

- **Claude** (`~/.claude/projects/<slug>/<id>.jsonl`):
  - `user` lines that aren't `isMeta` or `isCompactSummary`: string content, or
    `text` parts, become `user` items. `tool_result` parts complete their tool.
  - `assistant` lines: `text` becomes `assistant`, `thinking` becomes
    `reasoning`, `tool_use` becomes `tool`.
  - `system` lines with `subtype: "compact_boundary"` become `compaction`.
  - `custom-title` and `ai-title` update the session's titles.
  - Lines with `isSidechain: true` are skipped.
- **Codex** (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`), `response_item`
  payloads:
  - `message` with role `user` (skipping injected context) or `assistant`.
  - `reasoning` (its summary text).
  - `function_call`, `custom_tool_call`, `local_shell_call` and their
    `*_output`.
  - Titles come from `session_index.jsonl`, as before.
  - Its items have no line ids, so the session has no leaf and the history is
    simply the items in order.
- **Pi** (`<sessionDir>/--<path>--/*.jsonl`), `message` entries:
  - role `user` becomes `user`.
  - role `assistant`: `text`, `thinking` and `toolCall` become `assistant`,
    `reasoning` and `tool`.
  - role `toolResult` completes its tool.
  - `title`, `session_info` and `title_change` update the session's titles.
  - Its first line may be rewritten in place to carry a title, so it is read
    again on every incremental read.

## Flows

### Add a project

A button in the sidebar footer opens a folder picker (`cx.prompt_for_paths`
with `directories: true`, `files: false`, `multiple: false`). The folder is
inserted into `projects`. Adding a folder that is already a project selects it.

### New chat

`+` on a project opens the agent picker, as before. agentshub inserts a `chats`
row, then starts the agent in the project folder. Its first session report adds
the chat's first session.

### Open a chat

If its CLI is running, show its terminal, as before. Otherwise:

1. Read the chat's last session, since it decides how the CLI starts.
2. Show the earlier sessions above the terminal. Their items are read when a
   row is expanded.
3. Resume the last session with `Agent::resume_args`. A last session without a
   prompt has nothing to resume, so a new session starts instead.

When a chat's CLI exits, its terminal stays visible so startup and exit errors
can be read, even if none of its sessions has a prompt. A restart button starts
the agent again. The chat can be deleted from the sidebar.

If the executable or project folder is unavailable, the saved history still
opens, including the last session. The startup error and restart button appear
above it.

### Missing transcripts

If the last session's transcript is gone, the chat starts a new session of the
same agent in the project folder instead. It is appended to the chat like any
other, and a banner above the terminal says the previous session couldn't be
resumed; a `notice` item in the database says the same thing in the history
later. Its content is still in the earlier sessions list. No summary of the
earlier sessions is passed to the new one.

### A session that another chat is running

Sessions are shared rows, so a chat can have resumed one that another chat's
CLI is still in. Resuming it again would put two CLIs on one transcript. The
chat opens with its history, the message above the terminal says which session
is busy, and the restart button starts a **new** session rather than resuming.

### Delete a chat or project

Rows only. A running CLI is stopped first; the folder and the CLIs' own files
stays.

### The database can't open

The window shows the reason and quits, instead of running on an in-memory
database that would silently keep nothing. The same goes for a second instance,
which says Agents Hub is already running.

## Interface

- **Sidebar**: projects from the database with their chats under them, newest
  first, each with its agent's lamp and status as before. Its footer has
  "Add project" and an export button. Projects can be removed, and chats
  deleted.
- **Chat**: the terminal, optionally with a notice banner above it, and the
  earlier sessions above that when there are any:
  - One row per earlier session: title, start time, and how it ended
    (cleared, replaced by `/resume`, forked, transcript gone).
  - Rows are collapsed. Expanding one reads its transcript and shows its last
    100 items, read-only: prompts, replies, collapsed reasoning, and tool calls
    showing their name and input, with their output behind a second expand.
    A button loads the 100 before those; the list isn't virtualised.
  - The whole list collapses to one line ("3 earlier sessions") so the terminal
    gets the room back.
  - A session that couldn't be read or loaded says so in its row.
- **Empty state**: explains adding a project, then starting a chat in it.

## Code changes

- `Cargo.toml`: `rusqlite` (`bundled`), and the `dev-fast` profile for quick
  loops.
- `src/db.rs`: opening, migrations (including the resolved-ancestry backfill),
  the queries, backups and export.
- `src/transcript.rs`: reading the three transcript formats into items,
  incrementally, with ancestry resolved as it saves.
- `src/history.rs`: the earlier sessions list, reading a session when its row
  is expanded, and paging.
- `src/reports.rs`: append-only reports, read incrementally, with the session's
  reason; falls back to the temp folder when the runtime folder can't be used.
- `src/workspace.rs`: chats keyed by their database id; the instance lock, the
  broken-database window, the live-session conflict, notices; opening resumes
  the last session. Codex chats still look for their session when its folder
  changes.
- `src/sidebar.rs`: lists projects and chats from the database, and the add
  project and export actions.
- `src/sessions.rs`: only what is still needed: Codex's folders,
  `codex_session_of`, which leaves out subagent threads itself, and
  `codex_thread_name`.

## Build order

The work was built in these steps, each leaving the app working:

1. Database and projects.
2. Chats, recording their first session, resuming the last on open, deleting.
3. Session tracking with append-only reports.
4. Transcripts for the three agents, with branches.
5. The history view.
6. Missing transcripts, cleanup, and schema 5's resolved ancestry.

## Tests

In the style of the existing tests:

- `transcript.rs`: per agent, fixture lines in, items out. Injected context is
  skipped, tool output joins its call and is cut, partial last lines wait,
  rewritten files are read again, and only the last branch shows.
- `reports.rs`: several reports appended, a report split across reads, a report
  spread over several lines.
- `db.rs`: an in-memory database. Session ordering (append, already last, move
  to end), deleting a chat deletes its unshared sessions only, paging, a read
  hanging off a line an earlier read dropped, export, and upgrading a schema-1
  database. Migration 5 was also run against a copy of a real database before
  release.

## Open questions

- How should Codex rollbacks show? Undo isn't handled yet.
- Should a chat be renameable? It is titled after its first titled session
  until then.
- Should existing CLI sessions be importable into chats? Out of scope; an
  existing install starts empty.
- Should search across chats exist? SQLite makes it a later migration (FTS5
  over `items`), not a redesign.

Answered: reasoning is kept in full; tool output is cut to 2 KB at each end; no
raw transcript is kept, so resume dies with the CLI's file, visibly.
