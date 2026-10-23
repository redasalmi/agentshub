//! Where Codex keeps its sessions, and which of them a Codex process is in.
//! Claude Code and pi report their sessions instead (see `reports`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

use crate::transcript::{self, clean_title};

const CODEX_INDEX: &str = "session_index.jsonl";

/// Codex's config directory, which its environment variable can point
/// elsewhere.
fn codex_home() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(std::env::home_dir()?.join(".codex")),
    }
}

/// The folder Codex stores its sessions in. `/proc/<pid>/fd` resolves
/// symlinks, so this does too, to match the files its processes keep open.
pub fn codex_sessions_dir() -> Option<PathBuf> {
    let dir = codex_home()?.join("sessions");
    Some(fs::canonicalize(&dir).unwrap_or(dir))
}

fn codex_index() -> Option<PathBuf> {
    Some(codex_home()?.join(CODEX_INDEX))
}

/// The Codex session a process is in: of the session files it keeps open, the
/// one written to last, leaving out subagent threads. Codex keeps the
/// sessions it switched away from open too. Resolve npm's Node launcher to its
/// native child first, then inspect only that process: a `codex exec` it
/// starts has a chat of its own.
#[cfg(target_os = "linux")]
pub fn codex_session_of(pid: u32) -> Option<PathBuf> {
    let dir = codex_sessions_dir()?;
    let process = codex_process_dir(&PathBuf::from(format!("/proc/{pid}")))?;
    let mut files: Vec<(SystemTime, PathBuf)> = fs::read_dir(process.join("fd"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .filter(|path| path.starts_with(&dir))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    // Newest first, so telling subagent threads apart opens one file, not all
    // of them.
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files
        .into_iter()
        .map(|(_, path)| path)
        .find(|path| !is_codex_subagent(path))
}

/// Codex keeps its sessions in files only Linux exposes the way this needs.
#[cfg(not(target_os = "linux"))]
pub fn codex_session_of(_: u32) -> Option<PathBuf> {
    None
}

/// Subagent threads record their parent as an object in their first line.
#[cfg(target_os = "linux")]
fn is_codex_subagent(path: &Path) -> bool {
    transcript::first_lines(path, 1)
        .first()
        .is_none_or(|meta| meta["type"] != "session_meta" || meta["payload"]["source"].is_object())
}

#[cfg(target_os = "linux")]
fn codex_process_dir(process: &Path) -> Option<PathBuf> {
    let exe = fs::read_link(process.join("exe")).ok()?;
    match exe.file_name()?.to_str()? {
        "node" | "nodejs" => {
            let children = fs::read_to_string(
                process
                    .join("task")
                    .join(process.file_name()?)
                    .join("children"),
            )
            .ok()?;
            children.split_whitespace().find_map(|pid| {
                let child = process.parent()?.join(pid);
                let exe = fs::read_link(child.join("exe")).ok()?;
                (exe.file_name()? == "codex").then_some(child)
            })
        }
        _ => Some(process.to_path_buf()),
    }
}

/// Codex keeps generated thread names in a separate append-only index, which
/// is only read again once it changed.
static THREAD_NAMES: Mutex<Option<Names>> = Mutex::new(None);

/// The index's length and modification time, and the names in it.
type Names = (u64, SystemTime, HashMap<String, String>);

pub fn codex_thread_name(agent_id: &str) -> Option<String> {
    let path = codex_index()?;
    let metadata = fs::metadata(&path).ok()?;
    let stamp = (metadata.len(), metadata.modified().ok()?);
    let mut names = THREAD_NAMES.lock().unwrap_or_else(|err| err.into_inner());
    if names
        .as_ref()
        .is_none_or(|(len, modified, _)| (*len, *modified) != stamp)
    {
        *names = Some((stamp.0, stamp.1, thread_names(&path)));
    }
    names
        .as_ref()
        .and_then(|(_, _, names)| names.get(agent_id).cloned())
}

fn thread_names(path: &Path) -> HashMap<String, String> {
    let Ok(index) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    index
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| {
            let id = value["id"].as_str()?.to_string();
            let name = clean_title(value["thread_name"].as_str()?)?;
            Some((id, name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolves_codex_launcher_without_following_exec_children() {
        let dir = temp_dir();
        // npm launcher -> native interactive CLI -> independent exec CLI.
        for (pid, exe, children) in [
            ("1", "node", "4 2"),
            ("2", "codex", "3"),
            ("3", "codex", ""),
            ("4", "other", ""),
            ("5", "nodejs", "2"),
            ("6", "node", "4"),
            ("7", "codex-linux-x64", "3"),
        ] {
            let process = dir.join(pid);
            fs::create_dir_all(process.join("task").join(pid)).unwrap();
            symlink(format!("/bin/{exe}"), process.join("exe")).unwrap();
            fs::write(process.join("task").join(pid).join("children"), children).unwrap();
        }
        assert_eq!(codex_process_dir(&dir.join("1")), Some(dir.join("2")));
        assert_eq!(codex_process_dir(&dir.join("2")), Some(dir.join("2")));
        assert_eq!(codex_process_dir(&dir.join("5")), Some(dir.join("2")));
        assert_eq!(codex_process_dir(&dir.join("6")), None);
        assert_eq!(codex_process_dir(&dir.join("7")), Some(dir.join("7")));
        assert_eq!(codex_process_dir(&dir.join("missing")), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
