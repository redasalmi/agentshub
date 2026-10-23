//! Reads the CLIs' transcripts into the trimmed copy of each session that the
//! database keeps: prompts, replies, reasoning, tool calls with their output
//! cut short, and compaction points. Context the CLIs inject, token counts,
//! images and other bookkeeping are left out.
//!
//! The CLIs only append to their transcripts, so each read starts where the
//! last one stopped and goes to the last complete line. A line still being
//! written is read next time.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Value, json};

use crate::agent::Agent;
use crate::db::{Db, Saved};
use crate::sessions;

const TITLE_CHARS: usize = 80;

/// How much of a tool's output is kept at each end.
const OUTPUT_KEEP: usize = 2048;

/// What stands in for an image.
const IMAGE: &str = "[image]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Compaction,
    /// Something agentshub says, like that a transcript is gone.
    Notice,
}

impl Kind {
    const ALL: [Kind; 6] = [
        Kind::User,
        Kind::Assistant,
        Kind::Reasoning,
        Kind::Tool,
        Kind::Compaction,
        Kind::Notice,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Assistant => "assistant",
            Kind::Reasoning => "reasoning",
            Kind::Tool => "tool",
            Kind::Compaction => "compaction",
            Kind::Notice => "notice",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str() == kind)
    }
}

pub struct Item {
    /// The id of the transcript line it comes from. A line that makes several
    /// items gives its id to the last, and the others get theirs from it.
    pub source_id: Option<String>,
    pub parent_id: Option<String>,
    pub kind: Kind,
    pub at: Option<i64>,
    pub payload: Value,
}

/// The output of a tool called before this read.
pub struct Output {
    pub call_id: String,
    pub output: String,
    pub truncated: bool,
    pub error: bool,
}

/// What one read of a transcript found.
#[derive(Default)]
pub struct Read {
    /// Whether the file was rewritten, so what was read of it before goes.
    pub reset: bool,
    /// Where the first line that hasn't been read starts.
    pub offset: u64,
    pub items: Vec<Item>,
    pub outputs: Vec<Output>,
    /// Every line read, with its parent, kept or not. The database walks it to
    /// find the kept ancestor a new item hangs off.
    pub parents: HashMap<String, Option<String>>,
    /// The ids of the lines read, in order. The last one is `leaf`, unless
    /// the transcript replayed an older line after a new one.
    pub(crate) lines: Vec<String>,
    /// The last entry read, whose branch the session ended on. It can be a
    /// line that kept no item, such as a branch summary.
    pub leaf: Option<String>,
    /// The first prompt's text, if it had any to title the session with.
    pub prompt: Option<String>,
    /// Whether the user prompted the session at all, even with only an image.
    pub prompted: bool,
    /// Titles that were set, as `Some(None)` where one was cleared.
    pub custom_title: Option<Option<String>>,
    pub generated_title: Option<Option<String>>,
    /// The tool calls in `items`, by their call ids.
    calls: HashMap<String, usize>,
}

/// Reads the lines added to a session's transcript since the last read, and
/// says whether the history shows anything new.
pub fn read(db: &Db, session_id: i64) -> anyhow::Result<bool> {
    loop {
        let Some(transcript) = db.transcript(session_id)? else {
            return Ok(false);
        };
        match Read::file(
            transcript.agent,
            &transcript.agent_id,
            &transcript.path,
            transcript.offset,
        ) {
            Ok(read) => match db.save_read_state(session_id, transcript.offset, &read)? {
                // This read started where another one did, but the other one
                // saved first. Read from where it stopped, so the lines only
                // this read saw aren't lost.
                Saved::Stale => continue,
                saved => return Ok(saved.changed()),
            },
            // Its items stay.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                db.set_missing(session_id)?;
                return Ok(false);
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// The CLI's id for the session in a transcript. Claude and pi name their
/// files after it, and Codex keeps it in its first line. Pi keeps it in its
/// header too, which an extension's title can come before.
pub fn session_id(agent: Agent, path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    match agent {
        Agent::Claude => Some(stem.into_owned()),
        Agent::Codex => {
            let meta = first_lines(path, 1).pop()?;
            meta["payload"]["id"].as_str().map(String::from)
        }
        // Pi doesn't write its file before the session's first reply.
        Agent::Pi => first_lines(path, 2)
            .into_iter()
            .find(|line| line["type"] == "session")
            .and_then(|header| header["id"].as_str().map(String::from))
            // The file is named `<time>_<id>`; an id may contain an underscore.
            .or_else(|| stem.split_once('_').map(|(_, id)| id.to_string())),
    }
}

pub fn first_lines(path: &Path, count: usize) -> Vec<Value> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .take(count)
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect()
}

impl Read {
    fn file(agent: Agent, agent_id: &str, path: &Path, mut offset: u64) -> io::Result<Self> {
        let mut read = Read::default();
        let mut file = File::open(path)?;
        if offset > file.metadata()?.len() {
            read.reset = true;
            offset = 0;
        }
        let mut line = Vec::new();
        // An extension can keep a title on pi's first line, rewritten in
        // place and padded so the lines after it don't move.
        if agent == Agent::Pi && offset > 0 {
            BufReader::new(&file).read_until(b'\n', &mut line)?;
            // Only a title is taken from the line read again, not the branch
            // the session ended on.
            let leaf = read.leaf.take();
            let lines = read.lines.len();
            read.line(agent, &line);
            read.leaf = leaf;
            read.lines.truncate(lines);
        }

        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);
        loop {
            line.clear();
            let len = reader.read_until(b'\n', &mut line)?;
            if line.last() != Some(&b'\n') {
                break;
            }
            offset += len as u64;
            read.line(agent, &line);
        }
        read.offset = offset;

        // Codex keeps generated titles in an index of its own.
        if agent == Agent::Codex
            && let Some(title) = sessions::codex_thread_name(agent_id)
        {
            read.generated_title = Some(Some(title));
        }
        Ok(read)
    }

    fn line(&mut self, agent: Agent, line: &[u8]) {
        let Ok(line) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        match agent {
            Agent::Claude => self.claude_line(&line),
            Agent::Codex => self.codex_line(&line),
            Agent::Pi => self.pi_line(&line),
        }
    }

    fn claude_line(&mut self, line: &Value) {
        // Subagents' lines, which older versions kept in the same file.
        if line["isSidechain"] == true {
            return;
        }
        let content = &line["message"]["content"];
        let mut items = Vec::new();
        match line["type"].as_str() {
            Some("user") => {
                for part in content.as_array().into_iter().flatten() {
                    if part["type"] == "tool_result"
                        && let Some(call_id) = part["tool_use_id"].as_str()
                    {
                        self.complete(call_id, &part["content"], part["is_error"] == true);
                    }
                }
                if line["isMeta"] != true && line["isCompactSummary"] != true {
                    items.extend(self.prompt(content, &INJECTED_PROMPTS));
                }
            }
            Some("assistant") => {
                for part in content.as_array().into_iter().flatten() {
                    items.extend(match part["type"].as_str() {
                        Some("text") => text_item(Kind::Assistant, &part["text"]),
                        Some("thinking") => text_item(Kind::Reasoning, &part["thinking"]),
                        Some("tool_use") => tool_item(&part["id"], &part["name"], &part["input"]),
                        _ => None,
                    });
                }
            }
            Some("system") if line["subtype"] == "compact_boundary" => {
                items.push((Kind::Compaction, json!({})));
            }
            Some("custom-title") => {
                if let Some(title) = line["customTitle"].as_str().and_then(clean_title) {
                    self.custom_title = Some(Some(title));
                }
            }
            Some("ai-title") => {
                if let Some(title) = line["aiTitle"].as_str().and_then(clean_title) {
                    self.generated_title = Some(Some(title));
                }
            }
            _ => {}
        }
        // A compaction starts the branch over, and remembers where it was.
        let parent = line["parentUuid"]
            .as_str()
            .or(line["logicalParentUuid"].as_str());
        self.push_line(line["uuid"].as_str(), parent, &line["timestamp"], items);
    }

    fn codex_line(&mut self, line: &Value) {
        let payload = &line["payload"];
        let item = match (line["type"].as_str(), payload["type"].as_str()) {
            (Some("compacted"), _) => Some((Kind::Compaction, json!({}))),
            (Some("response_item"), Some(kind)) => match kind {
                "message" if payload["role"] == "user" => {
                    self.prompt(&payload["content"], &CODEX_INJECTED_PROMPTS)
                }
                "message" if payload["role"] == "assistant" => {
                    text_item(Kind::Assistant, &Value::String(texts(&payload["content"])))
                }
                "reasoning" => {
                    text_item(Kind::Reasoning, &Value::String(texts(&payload["summary"])))
                }
                "function_call" => {
                    let arguments = &payload["arguments"];
                    // Arguments come as JSON in a string.
                    let input = arguments
                        .as_str()
                        .and_then(|text| serde_json::from_str(text).ok())
                        .unwrap_or_else(|| arguments.clone());
                    tool_item(&payload["call_id"], &payload["name"], &input)
                }
                "custom_tool_call" => {
                    tool_item(&payload["call_id"], &payload["name"], &payload["input"])
                }
                "local_shell_call" => {
                    tool_item(&payload["call_id"], &"shell".into(), &payload["action"])
                }
                kind if kind.ends_with("_output") => {
                    if let Some(call_id) = payload["call_id"].as_str() {
                        self.complete(call_id, &payload["output"], false);
                    }
                    None
                }
                _ => None,
            },
            _ => None,
        };
        // Codex rollouts don't branch, so their lines have no ids.
        self.push_line(None, None, &line["timestamp"], item.into_iter().collect());
    }

    fn pi_line(&mut self, line: &Value) {
        let message = &line["message"];
        let mut items = Vec::new();
        match line["type"].as_str() {
            // The header isn't part of the tree.
            Some("session") => return,
            Some("title") => {
                self.generated_title = Some(line["title"].as_str().and_then(clean_title))
            }
            // Pi clears the name with an empty one.
            Some("session_info") => {
                self.custom_title = Some(line["name"].as_str().and_then(clean_title))
            }
            Some("title_change") => {
                if let Some(title) = line["title"].as_str().and_then(clean_title) {
                    self.custom_title = Some(Some(title));
                }
            }
            Some("compaction") => items.push((Kind::Compaction, json!({}))),
            Some("message") => match message["role"].as_str() {
                Some("user") => items.extend(self.prompt(&message["content"], &INJECTED_PROMPTS)),
                Some("assistant") => {
                    for part in message["content"].as_array().into_iter().flatten() {
                        items.extend(match part["type"].as_str() {
                            Some("text") => text_item(Kind::Assistant, &part["text"]),
                            Some("thinking") => text_item(Kind::Reasoning, &part["thinking"]),
                            Some("toolCall") => {
                                tool_item(&part["id"], &part["name"], &part["arguments"])
                            }
                            _ => None,
                        });
                    }
                }
                Some("toolResult") => {
                    if let Some(call_id) = message["toolCallId"].as_str() {
                        self.complete(call_id, &message["content"], message["isError"] == true);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        self.push_line(
            line["id"].as_str(),
            line["parentId"].as_str(),
            &line["timestamp"],
            items,
        );
    }

    /// Adds the items of one transcript line, or for a line without any, its
    /// place in the tree.
    fn push_line(
        &mut self,
        id: Option<&str>,
        parent: Option<&str>,
        at: &Value,
        items: Vec<(Kind, Value)>,
    ) {
        let at = at.as_str().and_then(parse_time);
        let Some(id) = id else {
            for (kind, payload) in items {
                self.push(None, None, kind, at, payload);
            }
            return;
        };
        self.lines.push(id.to_string());
        self.parents
            .insert(id.to_string(), parent.map(String::from));
        if items.is_empty() {
            self.leaf = Some(id.to_string());
            return;
        }
        let mut parent = parent.map(String::from);
        let last = items.len() - 1;
        for (index, (kind, payload)) in items.into_iter().enumerate() {
            let source_id = if index == last {
                id.to_string()
            } else {
                format!("{id}:{index}")
            };
            self.push(Some(source_id.clone()), parent, kind, at, payload);
            parent = Some(source_id);
        }
        self.leaf = Some(id.to_string());
    }

    fn push(
        &mut self,
        source_id: Option<String>,
        parent_id: Option<String>,
        kind: Kind,
        at: Option<i64>,
        payload: Value,
    ) {
        if let Some(call_id) = payload["id"].as_str().filter(|_| kind == Kind::Tool) {
            self.calls.insert(call_id.to_string(), self.items.len());
        }
        self.items.push(Item {
            source_id,
            parent_id,
            kind,
            at,
            payload,
        });
    }

    /// Writes a tool's output onto its call when this read has it, and always
    /// keeps it for the stored call too. A call line can be replayed along
    /// with its result: its item is then a duplicate that the database drops,
    /// so the output has to be persisted on its own.
    fn complete(&mut self, call_id: &str, content: &Value, error: bool) {
        let (output, truncated) = cut(output_text(content));
        if let Some(&index) = self.calls.get(call_id) {
            let payload = &mut self.items[index].payload;
            payload["output"] = output.clone().into();
            payload["truncated"] = truncated.into();
            payload["error"] = error.into();
        }
        self.outputs.push(Output {
            call_id: call_id.to_string(),
            output,
            truncated,
            error,
        });
    }

    /// The user item of a message `content`, if the user typed any of it.
    /// The first one titles the session.
    fn prompt(&mut self, content: &Value, injected: &[&str]) -> Option<(Kind, Value)> {
        let parts = prompt_parts(content, injected);
        if parts.is_empty() {
            return None;
        }
        // An image-only prompt has no text to title the session with, but it
        // is still a conversation.
        self.prompted = true;
        if self.prompt.is_none() {
            self.prompt = parts
                .iter()
                .filter(|part| *part != IMAGE)
                .find_map(|part| clean_title(part));
        }
        Some((Kind::User, json!({ "text": parts.join("\n\n") })))
    }
}

fn text_item(kind: Kind, text: &Value) -> Option<(Kind, Value)> {
    let text = text.as_str()?.trim();
    // Claude keeps only a signature of the thinking it doesn't show.
    (!text.is_empty()).then(|| (kind, json!({ "text": text })))
}

fn tool_item(call_id: &Value, name: &Value, input: &Value) -> Option<(Kind, Value)> {
    Some((
        Kind::Tool,
        json!({
            "id": call_id.as_str()?,
            "name": name.as_str().unwrap_or("tool"),
            "input": input,
            "output": null,
            "truncated": false,
            "error": false,
        }),
    ))
}

/// The text parts of a list of parts, as Codex has them.
fn texts(parts: &Value) -> String {
    parts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A tool's output, which is either text or a list of typed parts.
fn output_text(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part["type"].as_str() {
                Some("image" | "input_image") => Some(IMAGE),
                _ => part["text"].as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Keeps the first and last `OUTPUT_KEEP` bytes of a long output.
fn cut(text: String) -> (String, bool) {
    if text.len() <= 2 * OUTPUT_KEEP {
        return (text, false);
    }
    let head = text.floor_char_boundary(OUTPUT_KEEP);
    let tail = text.ceil_char_boundary(text.len() - OUTPUT_KEEP);
    (format!("{}\n…\n{}", &text[..head], &text[tail..]), true)
}

/// Seconds since the Unix epoch of an RFC 3339 time, such as
/// `2026-10-11T21:15:07.585Z`.
fn parse_time(text: &str) -> Option<i64> {
    let (date, time) = text.split_once('T')?;
    let mut date = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let number = |range: std::ops::Range<usize>| time.get(range)?.parse::<i64>().ok();
    let seconds = number(0..2)? * 3600 + number(3..5)? * 60 + number(6..8)?;
    let zone = time[8..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match zone.split_at_checked(1) {
        Some(("+" | "-", rest)) => {
            let (hours, minutes) = rest.split_once(':')?;
            let offset = hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60;
            if zone.starts_with('-') {
                -offset
            } else {
                offset
            }
        }
        _ => 0,
    };

    // Days since the epoch, from Howard Hinnant's `days_from_civil`.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + seconds - offset)
}

/// Context the CLIs inject as user messages, which is not something the user
/// typed. Only these wrappers are skipped, so prompts that happen to begin
/// with markup of their own are kept.
const INJECTED_PROMPTS: [&str; 13] = [
    "<system-reminder>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<command-name>",
    "<command-message>",
    "<command-args>",
    "<bash-input>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<user-prompt-submit-hook>",
    "<task-notification>",
    "<ide_opened_file>",
    "<ide_selection>",
];

/// Context Codex injects as user messages, which is not something the user
/// typed. Text that merely begins with markup, such as an HTML snippet, is
/// kept.
const CODEX_INJECTED_PROMPTS: [&str; 16] = [
    "# AGENTS.md",
    "<INSTRUCTIONS>",
    "<environment_context>",
    "<environments_instructions>",
    "<user_instructions>",
    "<collaboration_mode>",
    "<user_action>",
    "<turn_aborted>",
    "<skills_instructions>",
    "<plugins_instructions>",
    "<apps_instructions>",
    "<guardian_tool_descriptions>",
    "<skill>",
    "<image ",
    "</image>",
    "<user_query>",
];

/// The parts of a message `content` the user typed, which is either a string
/// or a list of typed parts. Skill invocations show as the skill, and Claude
/// records interruptions as user messages.
fn prompt_parts(content: &Value, injected: &[&str]) -> Vec<String> {
    let parts: Vec<&str> = match content {
        Value::String(text) => vec![text],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part["type"].as_str()? {
                "text" | "input_text" => part["text"].as_str(),
                "image" | "input_image" => Some(IMAGE),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    parts
        .into_iter()
        .filter_map(|text| {
            let text = text.trim();
            if let Some(rest) = text.strip_prefix("<skill name=\"") {
                return rest.split('"').next().map(|name| format!("/{name}"));
            }
            let skipped = text.is_empty()
                || injected.iter().any(|prefix| text.starts_with(prefix))
                || text.starts_with("[Request interrupted");
            (!skipped).then(|| text.to_string())
        })
        .collect()
}

pub fn clean_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(TITLE_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;

    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn append(path: &Path, lines: &[String]) {
        let mut file = File::options()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    /// A database with one chat in the session of `path`.
    fn session(agent: Agent, path: &Path) -> (Db, i64) {
        let db = Db::open_in_memory().unwrap();
        let project = db.add_project(Path::new("/p")).unwrap();
        let chat = db.add_chat(project, agent).unwrap();
        let id = session_id(agent, path).unwrap_or_else(|| "s".into());
        let session = db
            .record_session(chat, agent, &id, path, Some("startup"))
            .unwrap()
            .session_id;
        (db, session)
    }

    /// The kinds and texts of the items shown, a tool by its name and output.
    fn shown(db: &Db, session: i64) -> Vec<(Kind, String)> {
        read(db, session).unwrap();
        db.items(session, 100)
            .unwrap()
            .into_iter()
            .map(|item| {
                let payload = &item.payload;
                let text = match item.kind {
                    Kind::Tool => format!(
                        "{} -> {}",
                        payload["name"].as_str().unwrap(),
                        payload["output"].as_str().unwrap_or("?")
                    ),
                    _ => payload["text"].as_str().unwrap_or_default().to_string(),
                };
                (item.kind, text)
            })
            .collect()
    }

    fn title(db: &Db, session: i64) -> Option<String> {
        let chat = db.projects().unwrap().remove(0).chats.remove(0);
        db.sessions(chat.id)
            .unwrap()
            .into_iter()
            .find(|s| s.id == session)
            .unwrap()
            .title
    }

    fn claude(uuid: &str, parent: Option<&str>, kind: &str, content: Value) -> String {
        json!({
            "type": kind,
            "uuid": uuid,
            "parentUuid": parent,
            "timestamp": "2026-10-11T21:15:07.585Z",
            "message": { "role": kind, "content": content },
        })
        .to_string()
    }

    #[test]
    fn reads_claude_transcripts() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let long = format!("{}{}", "a".repeat(3000), "b".repeat(3000));
        append(
            &path,
            &[
                claude(
                    "1",
                    None,
                    "user",
                    json!("<command-name>/clear</command-name>"),
                ),
                claude("2", Some("1"), "user", json!("fix the bug\nplease")),
                json!({"type": "attachment", "uuid": "3", "parentUuid": "2"}).to_string(),
                claude(
                    "4",
                    Some("3"),
                    "assistant",
                    json!([
                        {"type": "thinking", "thinking": ""},
                        {"type": "text", "text": "Looking"},
                        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
                    ]),
                ),
                claude(
                    "5",
                    Some("4"),
                    "user",
                    json!([{"type": "tool_result", "tool_use_id": "t1", "content": long}]),
                ),
                json!({"type": "ai-title", "aiTitle": "Bug fix"}).to_string(),
                json!({"type": "assistant", "uuid": "6", "parentUuid": "5", "isSidechain": true,
                       "message": {"content": [{"type": "text", "text": "subagent"}]}})
                .to_string(),
            ],
        );
        let (db, session) = session(Agent::Claude, &path);
        let items = shown(&db, session);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], (Kind::User, "fix the bug\nplease".into()));
        assert_eq!(items[1], (Kind::Assistant, "Looking".into()));
        let output = &items[2].1;
        assert!(output.starts_with("Bash -> aaa") && output.ends_with("bbb"));
        assert!(output.len() < 4200, "{}", output.len());
        assert_eq!(title(&db, session).as_deref(), Some("Bug fix"));

        // A result that comes in a later read still finds its call, and a
        // line still being written waits.
        append(
            &path,
            &[claude(
                "7",
                Some("5"),
                "assistant",
                json!([
                    {"type": "tool_use", "id": "t2", "name": "Read", "input": {}},
                ]),
            )],
        );
        let mut file = File::options().append(true).open(&path).unwrap();
        write!(file, r#"{{"type":"user","uuid":"8","parentUuid":"7","message":{{"content":[{{"type":"tool_res"#).unwrap();
        assert_eq!(shown(&db, session)[3], (Kind::Tool, "Read -> ?".into()));
        writeln!(file, r#"ult","tool_use_id":"t2","content":[{{"type":"image"}},{{"type":"text","text":"done"}}]}}]}}}}"#).unwrap();
        // Claude can write a line again on resume.
        append(
            &path,
            &[claude("2", Some("1"), "user", json!("fix the bug\nplease"))],
        );
        let items = shown(&db, session);
        assert_eq!(items.len(), 4);
        assert_eq!(items[3], (Kind::Tool, "Read -> [image]\ndone".into()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_a_tool_output_when_its_call_line_is_replayed() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let call = claude(
            "1",
            None,
            "assistant",
            json!([{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]),
        );
        append(&path, std::slice::from_ref(&call));
        let (db, session) = session(Agent::Claude, &path);
        assert_eq!(shown(&db, session), [(Kind::Tool, "Bash -> ?".into())]);

        // Claude can replay the call line together with its result. The call
        // is already stored, but the output must still be saved.
        append(
            &path,
            &[
                call,
                claude(
                    "2",
                    Some("1"),
                    "user",
                    json!([{"type": "tool_result", "tool_use_id": "t1", "content": "done"}]),
                ),
            ],
        );
        assert_eq!(shown(&db, session), [(Kind::Tool, "Bash -> done".into())]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shows_the_branch_a_session_ended_on() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let reply = |uuid, parent, text| {
            claude(
                uuid,
                Some(parent),
                "assistant",
                json!([{"type": "text", "text": text}]),
            )
        };
        append(
            &path,
            &[
                claude("1", None, "user", json!("first")),
                reply("2", "1", "one"),
                json!({"type": "system", "subtype": "turn_duration", "uuid": "3", "parentUuid": "2"})
                    .to_string(),
                claude("4", Some("3"), "user", json!("abandoned")),
                reply("5", "4", "two"),
            ],
        );
        let (db, session) = session(Agent::Claude, &path);
        assert_eq!(shown(&db, session).len(), 4);

        // `/rewind` goes back to before the second prompt, past a line that
        // isn't kept and was read before.
        append(
            &path,
            &[
                claude("6", Some("3"), "user", json!("instead")),
                json!({"type": "system", "subtype": "compact_boundary", "uuid": "7",
                       "parentUuid": null, "logicalParentUuid": "6"})
                .to_string(),
                reply("8", "7", "three"),
            ],
        );
        let texts: Vec<String> = shown(&db, session)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(texts, ["first", "one", "instead", "", "three"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_rewritten_and_missing_files_again() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        append(
            &path,
            &[claude("1", None, "user", json!("a long first prompt"))],
        );
        let (db, session) = session(Agent::Claude, &path);
        assert_eq!(shown(&db, session).len(), 1);

        fs::write(
            &path,
            format!("{}\n", claude("2", None, "user", json!("new"))),
        )
        .unwrap();
        assert_eq!(shown(&db, session), [(Kind::User, "new".into())]);
        assert_eq!(title(&db, session).as_deref(), Some("new"));

        fs::remove_file(&path).unwrap();
        assert_eq!(shown(&db, session).len(), 1);
        let chat = db.projects().unwrap()[0].chats[0].id;
        assert!(db.sessions(chat).unwrap()[0].missing);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_codex_rollouts() {
        let dir = temp_dir();
        let path = dir.join("rollout.jsonl");
        let item =
            |payload: Value| json!({"type": "response_item", "payload": payload}).to_string();
        let message = |role: &str, kind: &str, text: &str| {
            item(
                json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]}),
            )
        };
        let agents_md = format!("# AGENTS.md\n{}", "x".repeat(270 * 1024));
        append(
            &path,
            &[
                r#"{"type":"session_meta","payload":{"id":"t1","cwd":"/p"}}"#.into(),
                message("developer", "input_text", "<permissions instructions>"),
                message("user", "input_text", &agents_md),
                message("user", "input_text", "fix the bug"),
                item(
                    json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "**Looking**"}]}),
                ),
                item(
                    json!({"type": "function_call", "call_id": "c1", "name": "exec", "arguments": "{\"cmd\":\"ls\"}"}),
                ),
                json!({"type": "event_msg", "payload": {"type": "token_count"}}).to_string(),
                item(json!({"type": "function_call_output", "call_id": "c1", "output": "a.txt"})),
                message("assistant", "output_text", "Fixed"),
            ],
        );
        let (db, session) = session(Agent::Codex, &path);
        assert_eq!(session_id(Agent::Codex, &path).as_deref(), Some("t1"));
        assert_eq!(
            shown(&db, session),
            [
                (Kind::User, "fix the bug".into()),
                (Kind::Reasoning, "**Looking**".into()),
                (Kind::Tool, "exec -> a.txt".into()),
                (Kind::Assistant, "Fixed".into()),
            ]
        );
        assert_eq!(title(&db, session).as_deref(), Some("fix the bug"));
        let tool = &db.items(session, 100).unwrap()[2].payload;
        assert_eq!(tool["input"]["cmd"], "ls");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_pi_sessions() {
        let dir = temp_dir();
        let path = dir.join("2026-01-01T00-00-00-000Z_s1.jsonl");
        let title_line = |title: &str| format!(r#"{{"type":"title","title":"{title:<10}"}}"#);
        let entry = |id: &str, parent: Option<&str>, message: Value| {
            json!({"type": "message", "id": id, "parentId": parent, "message": message}).to_string()
        };
        append(
            &path,
            &[
                title_line("Old"),
                r#"{"type":"session","id":"s1","cwd":"/p"}"#.into(),
                json!({"type": "model_change", "id": "m", "parentId": null}).to_string(),
                entry(
                    "a",
                    Some("m"),
                    json!({"role": "user", "content": [{"type": "text", "text": "<div>Fix this markup</div>"}]}),
                ),
                entry(
                    "b",
                    Some("a"),
                    json!({"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "Hmm"},
                        {"type": "toolCall", "id": "c1", "name": "read", "arguments": {"path": "x"}},
                    ]}),
                ),
                entry(
                    "c",
                    Some("b"),
                    json!({"role": "toolResult", "toolCallId": "c1",
                    "content": [{"type": "text", "text": "missing"}], "isError": true}),
                ),
                entry(
                    "d",
                    Some("c"),
                    json!({"role": "assistant", "content": [{"type": "text", "text": "Done"}]}),
                ),
            ],
        );
        let (db, session) = session(Agent::Pi, &path);
        assert_eq!(session_id(Agent::Pi, &path).as_deref(), Some("s1"));
        assert_eq!(
            shown(&db, session),
            [
                (Kind::User, "<div>Fix this markup</div>".into()),
                (Kind::Reasoning, "Hmm".into()),
                (Kind::Tool, "read -> missing".into()),
                (Kind::Assistant, "Done".into()),
            ]
        );
        assert_eq!(db.items(session, 100).unwrap()[2].payload["error"], true);
        assert_eq!(title(&db, session).as_deref(), Some("Old"));

        let mut contents = fs::read_to_string(&path).unwrap();
        contents.replace_range(..title_line("Old").len(), &title_line("New"));
        fs::write(&path, contents).unwrap();
        shown(&db, session);
        assert_eq!(title(&db, session).as_deref(), Some("New"));

        append(
            &path,
            &[r#"{"type":"session_info","id":"e","parentId":"d","name":"Mine"}"#.into()],
        );
        shown(&db, session);
        assert_eq!(title(&db, session).as_deref(), Some("Mine"));

        append(
            &path,
            &[r#"{"type":"session_info","id":"f","parentId":"e","name":""}"#.into()],
        );
        shown(&db, session);
        assert_eq!(title(&db, session).as_deref(), Some("New"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn counts_an_image_only_prompt_as_a_conversation() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        append(
            &path,
            &[claude(
                "1",
                None,
                "user",
                json!([{"type": "image", "source": {"type": "base64", "data": "..."}}]),
            )],
        );
        let (db, session) = session(Agent::Claude, &path);
        assert_eq!(shown(&db, session), [(Kind::User, IMAGE.into())]);
        // It has no text to title the session with, but it is still prompted.
        assert_eq!(title(&db, session), None);
        let chat = db.projects().unwrap()[0].chats[0].id;
        assert!(db.has_prompts(chat).unwrap());
        assert!(db.sessions(chat).unwrap()[0].prompted);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn follows_a_branch_summary_at_the_end_of_a_pi_transcript() {
        let dir = temp_dir();
        let path = dir.join("2026-01-01T00-00-00-000Z_s1.jsonl");
        let entry = |id: &str, parent: Option<&str>, message: Value| {
            json!({"type": "message", "id": id, "parentId": parent, "message": message}).to_string()
        };
        append(
            &path,
            &[
                r#"{"type":"session","id":"s1","cwd":"/p"}"#.into(),
                entry(
                    "a",
                    None,
                    json!({"role": "user", "content": [{"type": "text", "text": "first"}]}),
                ),
                entry(
                    "b",
                    Some("a"),
                    json!({"role": "assistant", "content": [{"type": "text", "text": "one"}]}),
                ),
                entry(
                    "c",
                    Some("b"),
                    json!({"role": "user", "content": [{"type": "text", "text": "abandoned"}]}),
                ),
                entry(
                    "d",
                    Some("c"),
                    json!({"role": "assistant", "content": [{"type": "text", "text": "two"}]}),
                ),
                // `/tree` went back to `b`; the summary is the last entry and
                // has no item of its own.
                r#"{"type":"branch_summary","id":"s","parentId":"b","summary":"back to one"}"#
                    .into(),
            ],
        );
        let (db, session) = session(Agent::Pi, &path);
        let texts: Vec<String> = shown(&db, session)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(texts, ["first", "one"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_the_newest_line_when_a_cold_read_ends_with_a_replay() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let first = claude("1", None, "user", json!("first"));
        let second = claude(
            "2",
            Some("1"),
            "assistant",
            json!([{"type": "text", "text": "second"}]),
        );
        // Claude replayed the first line after the second one. The second
        // message is still the newest one, and must stay on the branch.
        append(&path, &[first.clone(), second, first]);
        let (db, session) = session(Agent::Claude, &path);
        let texts: Vec<String> = shown(&db, session)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(texts, ["first", "second"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_the_newest_line_when_an_incremental_read_ends_with_a_replay() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let first = claude("1", None, "user", json!("first"));
        let second = claude(
            "2",
            Some("1"),
            "assistant",
            json!([{"type": "text", "text": "second"}]),
        );
        append(&path, std::slice::from_ref(&first));
        let (db, session) = session(Agent::Claude, &path);
        assert_eq!(shown(&db, session), [(Kind::User, "first".into())]);

        // The new line and a replay of the old one come in the same read.
        append(&path, &[second, first]);
        let texts: Vec<String> = shown(&db, session)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(texts, ["first", "second"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_branch_summary_after_a_read_changes_the_history() {
        let dir = temp_dir();
        let path = dir.join("2026-01-01T00-00-00-000Z_s1.jsonl");
        let entry = |id: &str, parent: Option<&str>, message: Value| {
            json!({"type": "message", "id": id, "parentId": parent, "message": message}).to_string()
        };
        append(
            &path,
            &[
                r#"{"type":"session","id":"s1","cwd":"/p"}"#.into(),
                entry(
                    "a",
                    None,
                    json!({"role": "user", "content": [{"type": "text", "text": "first"}]}),
                ),
                entry(
                    "b",
                    Some("a"),
                    json!({"role": "assistant", "content": [{"type": "text", "text": "one"}]}),
                ),
                entry(
                    "c",
                    Some("b"),
                    json!({"role": "user", "content": [{"type": "text", "text": "abandoned"}]}),
                ),
                entry(
                    "d",
                    Some("c"),
                    json!({"role": "assistant", "content": [{"type": "text", "text": "two"}]}),
                ),
            ],
        );
        let (db, session) = session(Agent::Pi, &path);
        assert_eq!(shown(&db, session).len(), 4);

        let texts = || {
            db.items(session, 100)
                .unwrap()
                .into_iter()
                .map(|item| {
                    item.payload["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        // `/tree` went back to `b`. Only the link is appended, but the
        // abandoned branch is no longer shown, so the history changed.
        append(
            &path,
            &[r#"{"type":"branch_summary","id":"s","parentId":"b","summary":"back"}"#.into()],
        );
        assert!(read(&db, session).unwrap());
        assert_eq!(texts(), ["first", "one"]);

        // A line that only extends the branch adds nothing to show, so the
        // history needn't be reloaded.
        append(
            &path,
            &[r#"{"type":"model_change","id":"m2","parentId":"s"}"#.into()],
        );
        assert!(!read(&db, session).unwrap());
        assert_eq!(texts(), ["first", "one"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-10-11T21:15:07.585Z"), Some(1_791_753_307));
        assert_eq!(parse_time("2026-10-11T23:15:07+02:00"), Some(1_791_753_307));
        assert_eq!(parse_time("yesterday"), None);
    }
}
