# Review rules

Failure here looks like:

- session history in SQLite lost or corrupted (migrations must be additive and tested)
- a CLI's PTY killed, orphaned, or leaking processes
- a git command run on the wrong worktree or branch
- the UI blocking on I/O on the foreground thread
- notifications that fire wrongly or not at all
- a workaround where the CLI has a native interface, or a feature outside the non-goals in CLAUDE.md

Driving the app:

- Use the verify skill (`.claude/skills/verify/`). Its `scripts/run.sh` runs a scenario against the dev build in a hidden KWin of its own, with its own session bus, `XDG_RUNTIME_DIR`, `XDG_DATA_HOME` and `HOME`, and fake CLIs first on a trimmed `PATH`. The developer's desktop, input, installed copy and real agents are never touched.
- A UI, chat, PTY, session or resume change runs the scenario that covers it, or adds one, and its screenshots get looked at: a `PASS` doesn't prove the screen looks right.
- A change to the skill keeps it isolated: never `kwin_wayland` without `--virtual`, never the developer's own `kwinrc`, and never stopping a test copy by process name or binary path, which match the installed copy too.
- If the hidden session can't run, ask the developer before driving their real desktop.

Rules:

- Cite file:line for every finding. Never guess from a name.
- At most 3 nits.
- Tag bugs the change didn't introduce as pre-existing.
- Skip `target/`, `Cargo.lock`, `assets/`.
- Start the summary with the finding count. Say "nothing to fix" plainly when clean.
