//! Claude Code and pi report the session they are in, at startup and whenever
//! they switch to another one (`/clear`, `/new`, `/resume`), into a file per
//! terminal: Claude through a SessionStart hook, pi through an extension. Codex
//! only runs hooks the user has reviewed, so its session is found from the
//! files it keeps open instead (see `sessions::codex_session_of`).

use std::fs;
use std::path::PathBuf;

use notify::{RecursiveMode, Watcher as _};
use serde_json::Value;

use crate::agent::Agent;

pub const ENV: &str = "AGENTSHUB_REPORT";

const CLAUDE_SETTINGS: &str = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"cat > \"$AGENTSHUB_REPORT\""}]}]}}"#;

const PI_EXTENSION_FILE: &str = "pi-extension.js";
const PI_EXTENSION: &str = r#"import { writeFileSync } from "node:fs";

export default function (pi) {
  pi.on("session_start", (_event, ctx) => {
    const file = ctx.sessionManager.getSessionFile();
    if (process.env.AGENTSHUB_REPORT && file) {
      writeFileSync(process.env.AGENTSHUB_REPORT, JSON.stringify({ transcript_path: file }));
    }
  });
}
"#;

pub struct Reports {
    dir: PathBuf,
    ready: bool,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Reports {
    /// Also returns the names of the reports as they are written.
    pub fn new() -> (Self, smol::channel::Receiver<String>) {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "agentshub-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        let ready = builder
            .create(&dir)
            .and_then(|()| fs::write(dir.join(PI_EXTENSION_FILE), PI_EXTENSION))
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

    /// The session file the CLI in a terminal last reported. A report that is
    /// still being written doesn't parse and is read again once it's done.
    pub fn read(&self, key: &str) -> Option<PathBuf> {
        let report = fs::read_to_string(self.path(key)).ok()?;
        let report = serde_json::from_str::<Value>(&report).ok()?;
        report["transcript_path"].as_str().map(PathBuf::from)
    }

    pub fn remove(&self) {
        fs::remove_dir_all(&self.dir).ok();
    }
}
