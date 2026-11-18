# Architecture

Agents Hub is split into layers with one direction of dependency. The split is
enforced by the Cargo workspace (see `Cargo.toml`) and by
`scripts/check-arch.sh`.

```
bin ──▶ ui ──▶ app ──▶ core
          └──▶ platform
```

- **core** — domain and backend. No `gpui_kit`, `Window`, `Entity`, `Context`
  or `App`. It owns the database, migrations, transcripts, sessions, reports,
  worktrees, CLI process control and the chat lifecycle state machine.
- **app** — use cases and backend orchestration. It owns the catalog, alert
  policy and sound, settings, persistence of settings and startup. It uses
  the platform notification transport and takes a callback for what a click
  should open. It may use GPUI entities and window handles for dialogs, but
  never builds element trees.
- **ui** — the GPUI frontend: themes, sidebar, search palette, settings,
  history, terminal model/view, chat sheet, toolbar, footer, empty state, and
  the workspace controller. Views read app state and emit intents; the
  workspace controller uses the core store API and the platform adapters. It
  reaches systemd only through the `Scope` port the binary installs.
- **platform** — Linux adapters: tray, D-Bus notifications, systemd scope,
  desktop motion, single instance and SIGTERM. It calls back into the app or
  UI for window and workspace actions instead of naming them.
- **bin** — composition root: starts the app, opens the window, wires the
  scopes, notifications and tray.

## Where things belong

| New thing | Layer |
| --- | --- |
| SQL, migration, query | `core::store` |
| Transcript parser or CLI session discovery | `core::transcript`, `core::sessions` |
| Git worktree or process control | `core::worktree`, `core::process` |
| State machine for open/stop/delete races | `core::lifecycle` |
| Use case that touches the database and the UI | `app` |
| Button, row, pane, dialog | `ui` |
| Workspace controller or open-chat flow | `ui::workspace` |
| Tray, D-Bus, systemd | `platform` |

## Rules

1. `core` has no UI dependency.
2. `ui` views never talk to the database or the filesystem; they read app
   state and emit intents. The `ui::workspace` controller may use the core
   store API, but no SQL and no `rusqlite`.
3. One owner per piece of state. Catalog data lives in `app`, not in a view.
4. Writes run in the order asked for; reads see the writes asked for before
   them (see `core::store`).
5. The database schema and migration order are append-only.
6. Refactors do not change user-visible behavior, strings, shortcuts or UI.

## Remaining follow-up

The workspace controller still owns both runtime state and view entities.
Splitting `ChatRuntime` from `ChatPane` would let the runtime move to `app`
and leave `ui` with only rendering. This is a follow-up, not a layer
violation: the controller already uses only the core store API.

## Verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-arch.sh
```

UI, chat, PTY, session and resume changes also run the `verify` skill.
