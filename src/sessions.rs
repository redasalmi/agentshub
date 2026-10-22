//! Reads the sessions the CLIs already store on disk. Nothing is written.
//!
//! Session files can be many megabytes. The CLIs only append to them, so each
//! file is read once and later rescans only read the lines added since.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::agent::Agent;

const TITLE_CHARS: usize = 80;
const CODEX_INDEX: &str = "session_index.jsonl";

/// Each CLI's config directory, which its environment variable can point
/// elsewhere.
fn config_dir(agent: Agent) -> Option<PathBuf> {
    let (var, fallback) = match agent {
        Agent::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
        Agent::Codex => ("CODEX_HOME", ".codex"),
        Agent::Pi => ("PI_CODING_AGENT_DIR", ".pi/agent"),
    };
    match std::env::var_os(var).filter(|value| !value.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(std::env::home_dir()?.join(fallback)),
    }
}

fn expand_tilde(path: PathBuf) -> Option<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => Some(std::env::home_dir()?.join(rest)),
        Err(_) => Some(path),
    }
}

/// The folder an agent stores its sessions in, as the agent itself resolves it.
fn sessions_dir(agent: Agent) -> Option<PathBuf> {
    match agent {
        Agent::Claude => Some(config_dir(agent)?.join("projects")),
        Agent::Codex => {
            // `/proc/<pid>/fd` resolves symlinks, so list Codex sessions under
            // their real path to match the files its processes keep open.
            let dir = config_dir(agent)?.join("sessions");
            Some(fs::canonicalize(&dir).unwrap_or(dir))
        }
        Agent::Pi => pi_sessions_dir(),
    }
}

/// Pi's session folder, which its environment variable or global setting can
/// point elsewhere. Its setting wins over the default but loses to the
/// environment variable.
fn pi_sessions_dir() -> Option<PathBuf> {
    if let Some(dir) =
        std::env::var_os("PI_CODING_AGENT_SESSION_DIR").filter(|value| !value.is_empty())
    {
        return expand_tilde(PathBuf::from(dir));
    }
    let config = expand_tilde(config_dir(Agent::Pi)?)?;
    let configured = fs::read_to_string(config.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|settings| settings["sessionDir"].as_str().map(PathBuf::from))
        .filter(|dir| !dir.as_os_str().is_empty());
    match configured {
        Some(dir) => expand_tilde(dir),
        None => Some(config.join("sessions")),
    }
}

fn codex_index() -> Option<PathBuf> {
    Some(config_dir(Agent::Codex)?.join(CODEX_INDEX))
}

#[derive(Clone, Debug)]
pub struct Session {
    pub agent: Agent,
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub title: String,
    pub updated: SystemTime,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub path: PathBuf,
    pub name: String,
    pub sessions: Vec<Session>,
}

impl Project {
    pub fn new(path: PathBuf, sessions: Vec<Session>) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            path,
            name,
            sessions,
        }
    }
}

/// What has been read of each session file, so a rescan only reads the lines
/// added since the last one.
#[derive(Default)]
pub struct SessionStore {
    cache: HashMap<PathBuf, SessionFile>,
    codex_titles: HashMap<String, String>,
}

impl SessionStore {
    /// Lists the session files again and reads those that changed.
    pub fn scan(&mut self) -> Vec<Project> {
        let mut cache = HashMap::with_capacity(self.cache.len());
        for agent in Agent::ALL {
            let Some(dir) = sessions_dir(agent) else {
                continue;
            };
            for path in session_files(agent, &dir) {
                let file = self
                    .cache
                    .remove(&path)
                    .unwrap_or_else(|| SessionFile::new(agent, &path));
                cache.insert(path, file);
            }
        }
        self.cache = cache;
        let paths: Vec<PathBuf> = self.cache.keys().cloned().collect();
        for path in &paths {
            self.update(path);
        }
        self.codex_titles = codex_thread_names();
        self.projects()
    }

    /// Reads only the files among `paths` that the last scan listed. Others,
    /// like subagent transcripts, aren't sessions.
    pub fn refresh(&mut self, paths: &HashSet<PathBuf>) -> Vec<Project> {
        for path in paths {
            self.update(path);
        }
        if codex_index().is_some_and(|index| paths.contains(&index)) {
            self.codex_titles = codex_thread_names();
        }
        self.projects()
    }

    /// Session files that aren't chats of their own, like Codex subagent
    /// threads.
    pub fn hidden(&self) -> HashSet<PathBuf> {
        self.cache
            .iter()
            .filter(|(_, file)| file.hidden)
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn update(&mut self, path: &Path) {
        if !self.cache.contains_key(path) {
            return;
        }
        let Ok((len, modified)) =
            fs::metadata(path).and_then(|metadata| Ok((metadata.len(), metadata.modified()?)))
        else {
            self.cache.remove(path);
            return;
        };
        let file = self.cache.get_mut(path).unwrap();
        // A file shorter than what was read has been rewritten.
        if file.offset > len {
            *file = SessionFile::new(file.agent, path);
        } else if file.modified == Some(modified) {
            return;
        }
        match file.read(path) {
            // Only a complete read counts, so a failed one is tried again.
            Ok(()) => file.modified = Some(modified),
            Err(err) => eprintln!("can't read {}: {err}", path.display()),
        }
    }

    fn projects(&self) -> Vec<Project> {
        let mut projects: HashMap<PathBuf, Vec<Session>> = HashMap::new();
        for (path, file) in &self.cache {
            let Some(mut session) = file.session(path) else {
                continue;
            };
            if let Some(title) = self.codex_titles.get(&session.id) {
                session.title = title.clone();
            }
            // Sessions without a single prompt (opened and closed, or only
            // `/clear`) have nothing to resume.
            if session.title.is_empty() {
                continue;
            }
            projects
                .entry(session.cwd.clone())
                .or_default()
                .push(session);
        }

        let mut projects: Vec<Project> = projects
            .into_iter()
            .map(|(path, mut sessions)| {
                sessions.sort_by_key(|session| Reverse(session.updated));
                Project::new(path, sessions)
            })
            .collect();
        projects.sort_by_key(|project| Reverse(project.sessions[0].updated));
        projects
    }
}

/// Session folders and the other files whose changes show up in the list.
pub fn watched_paths() -> Vec<PathBuf> {
    Agent::ALL
        .iter()
        .filter_map(|&agent| sessions_dir(agent))
        .chain(codex_index())
        .collect()
}

/// The Codex session a process is in: of the session files it keeps open, the
/// one written to last, leaving out the `hidden` ones. Codex keeps the
/// sessions it switched away from open too. Resolve npm's Node launcher to its
/// native child first, then inspect only that process: a `codex exec` it
/// starts has a chat of its own.
pub fn codex_session_of(pid: u32, hidden: &HashSet<PathBuf>) -> Option<PathBuf> {
    let dir = sessions_dir(Agent::Codex)?;
    let process = codex_process_dir(&PathBuf::from(format!("/proc/{pid}")))?;
    fs::read_dir(process.join("fd"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .filter(|path| path.starts_with(&dir))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter(|path| !hidden.contains(path))
        .max_by_key(|path| fs::metadata(path).and_then(|m| m.modified()).ok())
}

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

/// The session files of one CLI. Files nested deeper than these are subagent
/// transcripts, which are not chats of their own.
fn session_files(agent: Agent, dir: &Path) -> Vec<PathBuf> {
    match agent {
        Agent::Claude => jsonl_files(dir, 1),
        Agent::Codex => jsonl_files(dir, 3),
        // Pi keeps a folder per project, named after its path like
        // `--home-me-project--`. Older versions, and a custom `sessionDir`,
        // put sessions directly in the root instead.
        Agent::Pi => {
            let mut files = jsonl_files(dir, 0);
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.len() > 4 && name.starts_with("--") && name.ends_with("--") {
                    files.extend(jsonl_files(&entry.path(), 0));
                }
            }
            files
        }
    }
}

/// `.jsonl` files exactly `depth` directories below `dir`.
fn jsonl_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if depth == 0 {
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        } else if path.is_dir() {
            files.extend(jsonl_files(&path, depth - 1));
        }
    }
    files
}

struct SessionFile {
    agent: Agent,
    /// When the file had been modified as of the last read.
    modified: Option<SystemTime>,
    /// Where the first line that hasn't been read starts.
    offset: u64,
    id: Option<String>,
    cwd: Option<PathBuf>,
    first_prompt: Option<String>,
    /// The latest title the CLI generated.
    generated_title: Option<String>,
    /// The latest title the user gave the session, which wins over the others.
    custom_title: Option<String>,
    /// Codex subagent threads, which are not chats of their own.
    hidden: bool,
}

impl SessionFile {
    fn new(agent: Agent, path: &Path) -> Self {
        Self {
            agent,
            modified: None,
            offset: 0,
            // Claude names its files after the session.
            id: match agent {
                Agent::Claude => path.file_stem().map(|s| s.to_string_lossy().into_owned()),
                Agent::Codex | Agent::Pi => None,
            },
            cwd: None,
            first_prompt: None,
            generated_title: None,
            custom_title: None,
            hidden: false,
        }
    }

    fn session(&self, path: &Path) -> Option<Session> {
        if self.hidden {
            return None;
        }
        Some(Session {
            agent: self.agent,
            id: self.id.clone()?,
            path: path.to_path_buf(),
            cwd: self.cwd.clone()?,
            title: self
                .custom_title
                .as_ref()
                .or(self.generated_title.as_ref())
                .or(self.first_prompt.as_ref())
                .cloned()
                .unwrap_or_default(),
            updated: self.modified?,
        })
    }

    /// Everything Codex sessions show is at their start, while titles can be
    /// added to the others at any point.
    fn complete(&self) -> bool {
        self.agent == Agent::Codex && (self.hidden || self.first_prompt.is_some())
    }

    fn read(&mut self, path: &Path) -> io::Result<()> {
        let mut file = File::open(path)?;
        let mut line = Vec::new();
        // An extension can keep a title on pi's first line, rewritten in
        // place and padded so the lines after it don't move.
        if self.agent == Agent::Pi && self.offset > 0 {
            BufReader::new(&file).read_until(b'\n', &mut line)?;
            self.read_line(&String::from_utf8_lossy(&line));
        }
        if self.complete() {
            return Ok(());
        }

        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        loop {
            line.clear();
            let len = reader.read_until(b'\n', &mut line)?;
            // A line still being written is read again on the next change.
            if line.last() != Some(&b'\n') {
                return Ok(());
            }
            self.offset += len as u64;
            self.read_line(&String::from_utf8_lossy(&line));
            if self.complete() {
                return Ok(());
            }
        }
    }

    fn read_line(&mut self, line: &str) {
        match self.agent {
            Agent::Claude => self.read_claude_line(line),
            Agent::Codex => self.read_codex_line(line),
            Agent::Pi => self.read_pi_line(line),
        }
    }

    fn read_claude_line(&mut self, line: &str) {
        // Most lines are large assistant and tool records, so only those that
        // can matter are parsed.
        let searching = self.cwd.is_none() || self.first_prompt.is_none();
        if !line.contains("-title\"") && !(searching && line.contains("\"cwd\"")) {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match value["type"].as_str() {
            Some("custom-title") => {
                if let Some(title) = value["customTitle"].as_str() {
                    self.custom_title = Some(title.into());
                }
            }
            Some("ai-title") => {
                if let Some(title) = value["aiTitle"].as_str() {
                    self.generated_title = Some(title.into());
                }
            }
            Some("user") if self.first_prompt.is_none() && value["isMeta"] != true => {
                self.first_prompt = prompt_text(&value["message"]["content"]);
            }
            _ => {}
        }
        if self.cwd.is_none() {
            self.cwd = value["cwd"].as_str().map(PathBuf::from);
        }
    }

    fn read_codex_line(&mut self, line: &str) {
        if self.id.is_none() {
            let meta = serde_json::from_str::<Value>(line).unwrap_or_default();
            let payload = &meta["payload"];
            self.id = payload["id"].as_str().map(String::from);
            self.cwd = payload["cwd"].as_str().map(PathBuf::from);
            // Subagent threads record their parent as an object here.
            self.hidden = meta["type"] != "session_meta"
                || payload["source"].is_object()
                || self.id.is_none()
                || self.cwd.is_none();
            return;
        }
        if !line.contains("\"user\"") {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let payload = &value["payload"];
        if payload["type"] != "message" || payload["role"] != "user" {
            return;
        }
        // Injected context comes as user messages too, sometimes hundreds of
        // kilobytes of them before the prompt.
        self.first_prompt = payload["content"].as_array().and_then(|parts| {
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .find(|text| !is_codex_injected(text))
                .and_then(clean_title)
        });
    }

    fn read_pi_line(&mut self, line: &str) {
        let wanted = line.contains("\"type\":\"session")
            || line.contains("\"type\":\"title")
            || (self.first_prompt.is_none() && line.contains("\"type\":\"message\""));
        if !wanted {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match value["type"].as_str() {
            Some("session") => {
                self.id = value["id"].as_str().map(String::from);
                self.cwd = value["cwd"].as_str().map(PathBuf::from);
            }
            Some("title") => self.generated_title = value["title"].as_str().and_then(clean_title),
            Some("session_info") => {
                // Pi clears the name with an empty one.
                self.custom_title = value["name"].as_str().and_then(clean_title);
            }
            Some("title_change") => {
                if let Some(title) = value["title"].as_str().and_then(clean_title) {
                    self.custom_title = Some(title);
                }
            }
            Some("message")
                if self.first_prompt.is_none() && value["message"]["role"] == "user" =>
            {
                self.first_prompt = prompt_text(&value["message"]["content"]);
            }
            _ => {}
        }
    }
}

/// Codex keeps generated thread names in a separate append-only index.
fn codex_thread_names() -> HashMap<String, String> {
    let Some(index) = codex_index().and_then(|path| fs::read_to_string(path).ok()) else {
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

fn is_codex_injected(text: &str) -> bool {
    let text = text.trim_start();
    CODEX_INJECTED_PROMPTS
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// First user-typed text in a message `content`, which is either a string or
/// a list of typed parts. Skill invocations are titled after the skill, and
/// Claude records interruptions as user messages.
fn prompt_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(text) => text.as_str(),
        Value::Array(parts) => parts
            .iter()
            .find(|part| part["type"] == "text")?
            .get("text")?
            .as_str()?,
        _ => return None,
    };
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix("<skill name=\"") {
        return rest.split('"').next().map(|name| format!("/{name}"));
    }
    if INJECTED_PROMPTS
        .iter()
        .any(|prefix| text.starts_with(prefix))
        || text.starts_with("[Request interrupted")
    {
        return None;
    }
    clean_title(text)
}

fn clean_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(TITLE_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io::Write as _;
    use std::os::unix::fs::symlink;
    use std::process::Command;

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

    fn read(file: &mut SessionFile, path: &Path) -> Session {
        file.modified = Some(SystemTime::now());
        file.read(path).unwrap();
        file.session(path).unwrap()
    }

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

    #[test]
    fn honors_pi_session_directory_override() {
        // Run each environment in a subprocess so parallel tests never race
        // with changes to this process's environment.
        if let Some(expected) = std::env::var_os("AGENTSHUB_TEST_PI_DIR") {
            let expected = PathBuf::from(expected);
            assert_eq!(sessions_dir(Agent::Pi), Some(expected.clone()));
            assert!(watched_paths().contains(&expected));
            let projects = SessionStore::default().scan();
            assert_eq!(projects.len(), 1);
            let session = &projects[0].sessions[0];
            assert_eq!(session.agent, Agent::Pi);
            assert_eq!(session.path, expected.join("session.jsonl"));
            assert_eq!(session.title, "hello");
            return;
        }

        let dir = temp_dir();
        let config = dir.join("agent");
        let custom = dir.join("custom");
        let home = dir.join("home");
        let settings = config.join("settings.json");
        fs::create_dir_all(&config).unwrap();
        for (override_dir, setting, expected) in [
            (Some(custom.as_os_str()), None, custom.clone()),
            (None, Some(custom.as_os_str()), custom.clone()),
            (Some(OsStr::new("")), None, config.join("sessions")),
            (None, None, config.join("sessions")),
            (Some(OsStr::new("~/custom")), None, home.join("custom")),
            (None, Some(OsStr::new("~/custom")), home.join("custom")),
        ] {
            fs::create_dir_all(&expected).unwrap();
            fs::write(
                expected.join("session.jsonl"),
                concat!(
                    "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/p\"}\n",
                    "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
                ),
            )
            .unwrap();
            match setting {
                Some(value) => fs::write(
                    &settings,
                    serde_json::json!({ "sessionDir": value.to_string_lossy() }).to_string(),
                )
                .unwrap(),
                None => {
                    let _ = fs::remove_file(&settings);
                }
            }
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "sessions::tests::honors_pi_session_directory_override",
                ])
                .env("AGENTSHUB_TEST_PI_DIR", &expected)
                .env("HOME", &home)
                .env("PI_CODING_AGENT_DIR", &config)
                .env("CLAUDE_CONFIG_DIR", dir.join("claude"))
                .env("CODEX_HOME", dir.join("codex"));
            match override_dir {
                Some(value) => command.env("PI_CODING_AGENT_SESSION_DIR", value),
                None => command.env_remove("PI_CODING_AGENT_SESSION_DIR"),
            };
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_pi_sessions_in_project_folders() {
        let dir = temp_dir();
        let project = dir.join("--home-me-project--");
        let subagents = dir.join("2026-01-01T00-00-00-000Z_parent");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&subagents).unwrap();
        for path in [
            dir.join("legacy.jsonl"),
            project.join("current.jsonl"),
            subagents.join("Review.jsonl"),
        ] {
            File::create(path).unwrap();
        }

        let mut files = session_files(Agent::Pi, &dir);
        files.sort();
        assert_eq!(
            files,
            [project.join("current.jsonl"), dir.join("legacy.jsonl")]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_titles_as_transcripts_grow() {
        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let big = "x".repeat(70 * 1024);
        append(
            &path,
            &[
                r#"{"type":"user","cwd":"/p","message":{"content":"first prompt"}}"#.into(),
                r#"{"type":"custom-title","customTitle":"Renamed"}"#.into(),
                format!(r#"{{"type":"assistant","cwd":"/p","message":{{"content":"{big}"}}}}"#),
            ],
        );

        let mut file = SessionFile::new(Agent::Claude, &path);
        let session = read(&mut file, &path);
        assert_eq!(session.id, "abc");
        assert_eq!(session.cwd, Path::new("/p"));
        assert_eq!(session.title, "Renamed");

        append(
            &path,
            &[
                r#"{"type":"ai-title","aiTitle":"Generated"}"#.into(),
                format!(r#"{{"type":"assistant","cwd":"/p","message":{{"content":"{big}"}}}}"#),
            ],
        );
        assert_eq!(read(&mut file, &path).title, "Renamed");
        // A fresh read, as after a restart, finds it too.
        let mut file = SessionFile::new(Agent::Claude, &path);
        assert_eq!(read(&mut file, &path).title, "Renamed");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refreshes_listed_files_until_they_read() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_dir();
        let path = dir.join("abc.jsonl");
        let unlisted = dir.join("subagent.jsonl");
        let prompt = r#"{"type":"user","cwd":"/p","message":{"content":"hello"}}"#;
        append(&path, &[prompt.into()]);
        append(&unlisted, &[prompt.into()]);
        let mut store = SessionStore::default();
        store
            .cache
            .insert(path.clone(), SessionFile::new(Agent::Claude, &path));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = File::open(&path).is_err();
        let changed = HashSet::from([path.clone(), unlisted.clone()]);
        if unreadable {
            assert!(store.refresh(&changed).is_empty());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let projects = store.refresh(&changed);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].sessions.len(), 1);
        assert_eq!(projects[0].sessions[0].path, path);
        assert!(!store.cache.contains_key(&unlisted));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_codex_prompts_after_large_context() {
        let dir = temp_dir();
        let path = dir.join("rollout.jsonl");
        let agents_md = format!("# AGENTS.md\n{}", "x".repeat(270 * 1024));
        let message = |text: &str| {
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]},
            })
            .to_string()
        };
        append(
            &path,
            &[
                r#"{"type":"session_meta","payload":{"id":"t1","cwd":"/p"}}"#.into(),
                message(&agents_md),
                message("fix the bug"),
            ],
        );

        let mut file = SessionFile::new(Agent::Codex, &path);
        let session = read(&mut file, &path);
        assert_eq!(session.id, "t1");
        assert_eq!(session.title, "fix the bug");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rereads_pi_titles_rewritten_in_place() {
        let dir = temp_dir();
        let path = dir.join("session.jsonl");
        let title = |title: &str| format!(r#"{{"type":"title","title":"{title:<10}"}}"#);
        append(
            &path,
            &[
                title("Old"),
                r#"{"type":"session","id":"s1","cwd":"/p"}"#.into(),
                r#"{"type":"message","message":{"role":"user","content":"hello"}}"#.into(),
            ],
        );

        let mut file = SessionFile::new(Agent::Pi, &path);
        assert_eq!(read(&mut file, &path).title, "Old");

        let mut contents = fs::read_to_string(&path).unwrap();
        contents.replace_range(..title("Old").len(), &title("New"));
        fs::write(&path, contents).unwrap();
        assert_eq!(read(&mut file, &path).title, "New");

        append(&path, &[r#"{"type":"session_info","name":"Mine"}"#.into()]);
        assert_eq!(read(&mut file, &path).title, "Mine");

        append(&path, &[r#"{"type":"session_info","name":""}"#.into()]);
        assert_eq!(read(&mut file, &path).title, "New");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_pi_prompts_that_start_with_markup() {
        let dir = temp_dir();
        let path = dir.join("session.jsonl");
        append(
            &path,
            &[
                r#"{"type":"session","id":"s1","cwd":"/p"}"#.into(),
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"<div>Fix this markup</div>"}]}}"#.into(),
            ],
        );

        let mut file = SessionFile::new(Agent::Pi, &path);
        assert_eq!(read(&mut file, &path).title, "<div>Fix this markup</div>");
        fs::remove_dir_all(dir).unwrap();
    }
}
