use std::ffi::OsStr;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Pi];

    /// Short name, which is also what the database stores.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.label() == label)
    }

    /// The agents whose CLI is installed, which are the ones a new chat in
    /// `cwd` can start. Looked up each time, so a CLI installed or removed
    /// while agentshub runs is taken into account.
    pub fn installed(cwd: &Path) -> Vec<Self> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        Self::ALL
            .into_iter()
            .filter(|agent| on_path(agent.program(), &path, cwd))
            .collect()
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Pi => "Pi",
        }
    }

    pub fn program(self) -> &'static str {
        self.label()
    }

    pub fn resume_args(self, session_id: &str, session_path: &Path) -> Vec<String> {
        match self {
            Agent::Claude => vec!["--resume".into(), session_id.into()],
            Agent::Codex => vec!["resume".into(), session_id.into()],
            Agent::Pi => vec![
                "--session".into(),
                session_path.to_string_lossy().into_owned(),
            ],
        }
    }
}

/// Whether `program` is an executable file in one of the `path` directories,
/// which is where starting a CLI in `cwd` looks for it. As there, relative
/// directories are relative to `cwd`, and an empty one is `cwd` itself.
fn on_path(program: &str, path: &OsStr, cwd: &Path) -> bool {
    std::env::split_paths(path).any(|dir| is_executable(&cwd.join(dir).join(program)))
}

fn is_executable(file: &Path) -> bool {
    // Follows symlinks, as npm and other installers link their CLIs.
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn finds_executables_on_path() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let (a, b) = (temp_dir(), temp_dir());
        let executable = |file: &Path| {
            fs::write(file, "").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        };
        executable(&a.join("claude"));
        executable(&b.join("pi"));
        symlink(b.join("pi"), a.join("linked")).unwrap();
        symlink(b.join("missing"), a.join("dangling")).unwrap();
        fs::write(a.join("codex"), "").unwrap();
        fs::create_dir(b.join("dir")).unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        let elsewhere = Path::new("/");

        assert!(on_path("claude", &path, elsewhere));
        assert!(on_path("pi", &path, elsewhere));
        assert!(on_path("linked", &path, elsewhere));
        assert!(!on_path("dangling", &path, elsewhere));
        // Not executable, or not a file.
        assert!(!on_path("codex", &path, elsewhere));
        assert!(!on_path("dir", &path, elsewhere));
        assert!(!on_path("claude", OsStr::new(""), elsewhere));
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn resolves_path_entries_against_cwd() {
        use std::os::unix::fs::PermissionsExt as _;

        let project = temp_dir();
        fs::create_dir(project.join("bin")).unwrap();
        for file in [project.join("bin/claude"), project.join("pi")] {
            fs::write(&file, "").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Relative entries are relative to the project, as when it launches.
        assert!(on_path("claude", OsStr::new("bin:/usr/bin:/bin"), &project));
        assert!(!on_path("claude", OsStr::new("bin"), Path::new("/")));
        // An empty entry is the project itself.
        assert!(on_path("pi", OsStr::new("/usr/bin::/bin"), &project));
        assert!(on_path("pi", OsStr::new(""), &project));
        assert!(!on_path("pi", OsStr::new("/usr/bin:/bin"), &project));
        fs::remove_dir_all(project).unwrap();
    }
}
