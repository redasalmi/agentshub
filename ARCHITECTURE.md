# Architecture

Agents Hub is split into layers with one direction of dependency. The split is
enforced by the Cargo workspace (see `Cargo.toml`) and by
`scripts/check-arch.sh`.

```
bin ──▶ app ──▶ core
 │       ▲
 ├─────▶ ui ─────┘
 │
 └─────▶ platform
```

- **core** — domain and backend. No `gpui_kit`, `Window`, `Entity`, `Context`
  or `App`. It owns the database, migrations, transcripts, sessions, reports,
  worktrees, CLI process control and the chat lifecycle state machine.
- **app** — use cases and async orchestration. It owns the catalog, settings,
  persistence of settings and startup. It may use GPUI entities and window
  handles for dialogs, but never builds element trees.
- **ui** — rendering only. It owns the theme, sidebar, search palette and
  history view; the chat, settings and terminal views still live in the
  binary. Views read app state and emit intents. No `Store`, `rusqlite`,
  `std::fs` or `std::process`.
- **platform** — Linux adapters: tray, D-Bus notifications, systemd scope,
  desktop motion, single instance and SIGTERM. It calls back into the binary
  for window and workspace actions instead of naming them.
- **bin** — composition root: starts the app, opens the window, wires the
  layers, and still hosts the workspace controller and the remaining views
  while they move to `app` and `ui`.

## Where things belong

| New thing | Layer |
| --- | --- |
| SQL, migration, query | `core::store` |
| Transcript parser or CLI session discovery | `core::transcript`, `core::sessions` |
| Git worktree or process control | `core::worktree`, `core::process` |
| State machine for open/stop/delete races | `core::lifecycle` |
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

## Remaining migration

`workspace`, `chat_view`, `settings_view`, `terminal`, `terminal_view`,
`alerts` and `window` are still in the binary. Moving the views needs the
snapshot and intent boundary that rule 2 describes: views read a snapshot and
emit intents, and the workspace applies them. The terminal also needs its
systemd scope injected before it can move out of the binary.

## Verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-arch.sh
```

UI, chat, PTY, session and resume changes also run the `verify` skill.
