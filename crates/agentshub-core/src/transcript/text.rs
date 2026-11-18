//! Trimming the text, tool calls and times out of transcript lines.

use serde_json::{Value, json};

use super::*;

pub(super) fn text_item(kind: Kind, text: &Value) -> Option<(Kind, Value)> {
    let text = text.as_str()?.trim();
    // Claude keeps only a signature of the thinking it doesn't show.
    (!text.is_empty()).then(|| (kind, json!({ "text": text })))
}

pub(super) fn tool_item(call_id: &Value, name: &Value, input: &Value) -> Option<(Kind, Value)> {
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
pub(super) fn texts(parts: &Value) -> String {
    parts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A tool's output, which is either text or a list of typed parts.
pub(super) fn output_text(content: &Value) -> String {
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
#[expect(
    clippy::string_slice,
    reason = "`head` and `tail` are rounded to char boundaries"
)]
pub(super) fn cut(text: String) -> (String, bool) {
    if text.len() <= 2 * OUTPUT_KEEP {
        return (text, false);
    }
    let head = text.floor_char_boundary(OUTPUT_KEEP);
    let tail = text.ceil_char_boundary(text.len() - OUTPUT_KEEP);
    (format!("{}\n…\n{}", &text[..head], &text[tail..]), true)
}

/// Seconds since the Unix epoch of an RFC 3339 time, such as
/// `2026-10-11T21:15:07.585Z`.
pub(super) fn parse_time(text: &str) -> Option<i64> {
    text.parse::<jiff::Timestamp>()
        .ok()
        .map(|time| time.as_second())
}

/// Context the CLIs inject as user messages, which is not something the user
/// typed. Only these wrappers are skipped, so prompts that happen to begin
/// with markup of their own are kept.
pub(super) const INJECTED_PROMPTS: [&str; 13] = [
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
pub(super) const CODEX_INJECTED_PROMPTS: [&str; 16] = [
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
pub(super) fn prompt_parts(content: &Value, injected: &[&str]) -> Vec<String> {
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
                let name = rest.split('"').next()?;
                // The body is the skill's file, which can show the tag itself.
                let args = rest
                    .rsplit_once("</skill>")
                    .map_or("", |(_, args)| args.trim());
                return Some(if args.is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {args}")
                });
            }
            let skipped = text.is_empty()
                || injected.iter().any(|prefix| text.starts_with(prefix))
                || text.starts_with("[Request interrupted");
            (!skipped).then(|| text.to_string())
        })
        .collect()
}

/// What the user typed to run a command, from the line Claude writes for it.
/// Skills and prompt commands are expanded on the next line, while built-in
/// commands such as `/clear` are not.
pub(super) fn typed_command(content: &Value) -> Option<String> {
    let text = content.as_str()?.trim_start();
    if !text.starts_with("<command-message>") && !text.starts_with("<command-name>") {
        return None;
    }
    let name = tag(text, "command-name")?;
    Some(match tag(text, "command-args") {
        Some(args) if !args.is_empty() => format!("{name} {args}"),
        _ => name.to_string(),
    })
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(&format!("<{name}>"))?;
    let (inner, _) = rest.split_once(&format!("</{name}>"))?;
    Some(inner.trim())
}

pub fn clean_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(TITLE_CHARS).collect())
}
