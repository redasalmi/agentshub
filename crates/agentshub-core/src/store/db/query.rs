//! Reads that answer a search or save a transcript read.

use super::*;

impl Db {
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
    #[expect(
        clippy::too_many_lines,
        reason = "one transaction's steps, sharing its leaf, the kept ids and whether anything changed"
    )]
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
                read.offset,
                transcript::PARSER,
                update_leaf,
                leaf,
            ],
        )?;
        tx.commit()?;
        Ok(changed)
    }
}
