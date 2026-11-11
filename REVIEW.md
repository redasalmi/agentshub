# Review rules

Failure here looks like:

- session history in SQLite lost or corrupted (migrations must be additive and tested)
- a CLI's PTY killed, orphaned, or leaking processes
- a git command run on the wrong worktree or branch
- the UI blocking on I/O on the foreground thread
- notifications that fire wrongly or not at all
- a workaround where the CLI has a native interface, or a feature outside the non-goals in CLAUDE.md

Rules:

- Cite file:line for every finding. Never guess from a name.
- At most 3 nits.
- Tag bugs the change didn't introduce as pre-existing.
- Skip `target/`, `Cargo.lock`, `assets/`.
- Start the summary with the finding count. Say "nothing to fix" plainly when clean.
