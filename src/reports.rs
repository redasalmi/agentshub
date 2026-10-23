//! Claude Code and pi report the session they are in, at startup and whenever
//! they switch to another one (`/clear`, `/new`, `/resume`), by appending to a
//! file per terminal: Claude through a SessionStart hook, pi through an
//! extension. Codex only runs hooks the user has reviewed, so its session is
//! found from the files it keeps open instead (see `sessions::codex_session_of`).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;

use notify::{RecursiveMode, Watcher as _};
use serde_json::Value;

use crate::agent::Agent;
use crate::transcript;

pub const ENV: &str = "AGENTSHUB_REPORT";

const CLAUDE_SETTINGS: &str = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"cat >> \"$AGENTSHUB_REPORT\""}]}]}}"#;

const PI_EXTENSION_FILE: &str = "pi-extension.js";
const PI_EXTENSION: &str = r#"import { appendFileSync } from "node:fs";

export default function (pi) {
  pi.on("session_start", (event, ctx) => {
    const file = ctx.sessionManager.getSessionFile();
    if (process.env.AGENTSHUB_REPORT && file) {
      const report = { transcript_path: file, reason: event.reason };
      appendFileSync(process.env.AGENTSHUB_REPORT, JSON.stringify(report) + "\n");
    }
  });
}
"#;

/// A session a CLI went into.
#[derive(Debug, PartialEq)]
pub struct Report {
    pub transcript_path: PathBuf,
    /// Claude's id for the session. Pi's is in its transcript.
    pub session_id: Option<String>,
    /// Why the CLI went into it: Claude's `startup`, `resume`, `clear`,
    /// `compact` or `fork`, or pi's `startup`, `reload`, `new`, `resume` or
    /// `fork`.
    pub reason: Option<String>,
}

pub struct Reports {
    dir: PathBuf,
    ready: bool,
    /// How much of each terminal's reports has been read.
    read: HashMap<String, u64>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Reports {
    /// Also returns the names of the reports as they are written.
    pub fn new() -> (Self, smol::channel::Receiver<String>) {
        let name = format!("agentshub-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        // The runtime folder is private and wiped at logout, but it can be
        // missing or not writable, as in a sandboxed test run.
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(&name);
        let dir = match create_private_dir(&runtime) {
            Ok(()) => runtime,
            Err(_) => {
                // The folder may already exist from an earlier run of a
                // process that had this id.
                let dir = std::env::temp_dir().join(&name);
                create_private_dir(&dir).ok();
                dir
            }
        };
        let ready = fs::write(dir.join(PI_EXTENSION_FILE), PI_EXTENSION)
            .inspect_err(|err| eprintln!("can't set up {}: {err}", dir.display()))
            .is_ok();

        let (changed_tx, changed) = smol::channel::unbounded();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else {
                return;
            };
            // Reading a report opens it, which must not enqueue it again.
            if !matches!(
                event.kind,
                notify::EventKind::Create(_) | notify::EventKind::Modify(_)
            ) {
                return;
            }
            for path in event.paths {
                if let Some(name) = path.file_name() {
                    changed_tx
                        .try_send(name.to_string_lossy().into_owned())
                        .ok();
                }
            }
        })
        .and_then(|mut watcher| {
            watcher.watch(&dir, RecursiveMode::NonRecursive)?;
            Ok(watcher)
        })
        .inspect_err(|err| eprintln!("can't watch {}: {err}", dir.display()))
        .ok();

        (
            Self {
                dir,
                ready,
                read: HashMap::new(),
                _watcher: watcher,
            },
            changed,
        )
    }

    /// Arguments that make the CLI report its session to the file in `ENV`.
    pub fn args(&self, agent: Agent) -> Vec<String> {
        // pi doesn't start with a missing extension, and the hook would fail.
        if !self.ready {
            return Vec::new();
        }
        match agent {
            Agent::Claude => vec!["--settings".into(), CLAUDE_SETTINGS.into()],
            Agent::Codex => Vec::new(),
            Agent::Pi => vec![
                "--extension".into(),
                self.dir
                    .join(PI_EXTENSION_FILE)
                    .to_string_lossy()
                    .into_owned(),
            ],
        }
    }

    /// Where the CLI in the terminal with the given key reports to.
    pub fn path(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    /// The sessions the CLI in a terminal reported since the last read, each
    /// with the CLI's id for it.
    pub fn read(&mut self, key: &str, agent: Agent) -> Vec<(String, Report)> {
        let path = self.path(key);
        let offset = self.read.entry(key.to_string()).or_default();
        let mut bytes = Vec::new();
        let read = File::open(path).and_then(|mut file| {
            file.seek(SeekFrom::Start(*offset))?;
            file.read_to_end(&mut bytes)
        });
        if read.is_err() {
            return Vec::new();
        }
        let (reports, len) = parse(&bytes);
        *offset += len as u64;
        reports
            .into_iter()
            .filter_map(|report| {
                let agent_id = report
                    .session_id
                    .clone()
                    .or_else(|| transcript::session_id(agent, &report.transcript_path))?;
                Some((agent_id, report))
            })
            .collect()
    }

    pub fn remove(&self) {
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// Creates a folder only its user can read, for the reports it holds.
fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// The reports in what was appended, one JSON value after another, and how
/// much of it they take. A report that is still being written is read again
/// once it's done.
fn parse(bytes: &[u8]) -> (Vec<Report>, usize) {
    let mut values = serde_json::Deserializer::from_slice(bytes).into_iter::<Value>();
    let mut reports = Vec::new();
    let mut len = 0;
    loop {
        match values.next() {
            Some(Ok(value)) => {
                len = values.byte_offset();
                reports.extend(report(&value));
            }
            Some(Err(err)) if err.is_eof() => return (reports, len),
            // Garbage can't be told apart from what follows it, so it all goes.
            Some(Err(_)) | None => return (reports, bytes.len()),
        }
    }
}

fn report(value: &Value) -> Option<Report> {
    Some(Report {
        transcript_path: PathBuf::from(value["transcript_path"].as_str()?),
        session_id: value["session_id"].as_str().map(String::from),
        reason: value["source"]
            .as_str()
            .or(value["reason"].as_str())
            .map(String::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(reports: &[Report]) -> Vec<&str> {
        reports
            .iter()
            .map(|report| report.transcript_path.to_str().unwrap())
            .collect()
    }

    #[test]
    fn reads_appended_reports() {
        let claude = r#"{"session_id":"a","transcript_path":"/a.jsonl","source":"startup"}"#;
        let pi = "{\"transcript_path\":\"/b.jsonl\",\"reason\":\"new\"}\n";
        let bytes = format!("{claude}{claude}{pi}");
        let (reports, len) = parse(bytes.as_bytes());
        assert_eq!(len, bytes.len());
        assert_eq!(paths(&reports), ["/a.jsonl", "/a.jsonl", "/b.jsonl"]);
        assert_eq!(
            reports[2],
            Report {
                transcript_path: "/b.jsonl".into(),
                session_id: None,
                reason: Some("new".into()),
            }
        );
        assert_eq!(reports[0].session_id.as_deref(), Some("a"));
        assert_eq!(reports[0].reason.as_deref(), Some("startup"));
    }

    #[test]
    fn waits_for_reports_still_being_written() {
        let report = "{\n  \"session_id\": \"a\",\n  \"transcript_path\": \"/a.jsonl\"\n}\n";
        let bytes = format!("{report}{report}");
        for split in 0..bytes.len() {
            let (mut reports, len) = parse(&bytes.as_bytes()[..split]);
            let (rest, rest_len) = parse(&bytes.as_bytes()[len..]);
            reports.extend(rest);
            assert_eq!(len + rest_len, bytes.len(), "split at {split}");
            assert_eq!(
                paths(&reports),
                ["/a.jsonl", "/a.jsonl"],
                "split at {split}"
            );
        }
    }
}
