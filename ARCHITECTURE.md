# Architecture

Agents Hub is split into layers with one direction of dependency. The split is
enforced by the Cargo workspace (see `Cargo.toml`) and by
`scripts/check-arch.sh`.

```
bin ──▶ app ──▶ core
 │       │
 ├─────▶ ui ────▶ core
 │
 └─────▶ platform ──▶ core
```

- **core** — domain and backend. No `gpui_kit`, `Window`, `Entity`, `Context`
  or `App`. It owns the database, migrations, transcripts, sessions, reports,
  worktrees, settings model and the chat lifecycle state machine.
- **app** — use cases and async orchestration. It owns the catalog (projects
  and chats), open chat runtime, status, notifications policy and settings.
  It may use GPUI entities and window handles for dialogs, but never builds
  element trees.
- **ui** — rendering only. Views read snapshots and emit intents. No `Store`,
  `rusqlite`, `std::fs` or `std::process`.
- **platform** — Linux adapters: tray, D-Bus notifications, single instance,
  systemd scope, desktop motion.
- **bin** — composition root: starts the app, opens the window, wires the
  layers.

## Where things belong

| New thing | Layer |
| --- | --- |
| SQL, migration, query | `core::store` |
| Transcript parser or CLI session discovery | `core::transcript`, `core::sessions` |
| Git worktree or process control | `core::worktree`, `core::pty` |
| State machine for open/stop/delete races | `core::chat::lifecycle` |
| Use case that touches the database and the UI | `app` |
| Button, row, pane, dialog | `ui` |
| Tray, D-Bus, systemd | `platform` |

## Rules

1. `core` has no UI dependency.
2. `ui` never talks to the database or the filesystem; it asks `app`.
3. One owner per piece of state. Catalog data lives in `app`, not in a view.
4. Writes run in the order asked for; reads see the writes asked for before
   them (see `core::store`).
5. The database schema and migration order are append-only.
6. Refactors do not change user-visible behavior, strings, shortcuts or UI.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/check-arch.sh
```

UI, chat, PTY, session and resume changes also run the `verify` skill.
