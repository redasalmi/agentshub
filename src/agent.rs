use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Pi];

    /// Short name shown under each chat in the sidebar.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
        }
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
