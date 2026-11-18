# agentshub

A light Linux desktop home for running a handful of real agent CLIs (Claude, Codex, Pi) in terminals. Rust, GPUI (gpui-kit), libghostty-vt, SQLite (rusqlite), Wayland.

Non-goals: orchestration, fleet management, PR/CI/diff panes. Use each CLI's native interfaces, never workarounds.

## Verify before saying done

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Show the output. UI, chat, PTY, session and resume changes also get a run of the `verify` skill against a dev build, and a look at its screenshots.

`cargo run --profile dev-fast` rebuilds faster but runs slower.

@REVIEW.md
