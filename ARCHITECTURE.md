# Architecture

Agents Hub is split into layers with one direction of dependency. The split is
enforced by the Cargo workspace (see `Cargo.toml`) and by
`scripts/check-arch.sh`.

```
bin ──▶ ui-classic ──▶ ui-kit ──▶ runtime ──▶ app ──▶ core
 │          │            │
 │          └────────────┴──▶ platform
 └──▶ ui-kit
```

- **core** — domain and backend. No `gpui_kit`, `Window`, `Entity`, `Context`
  or `App`. It owns the database, migrations, transcripts, sessions, reports,
  worktrees, CLI process control and the chat lifecycle state machine.
- **app** — use cases and backend orchestration. It owns the catalog, alert
  policy and sound, settings, persistence of settings and startup. It uses
  the platform notification transport and takes a callback for what a click
  should open. It may use GPUI entities and window handles for dialogs, but
  never builds element trees.
- **runtime** — what runs behind the UI: chats, terminals, shells, the
  lifecycle, sessions and reports, startup and shutdown. GPUI entities, no
  element trees, no `Window`. It takes intents, tells the UI what changed
  through events, and reaches systemd only through the `Scope` port the
  binary installs.
- **ui-kit** — what every layout shares: theme and fonts, terminal view,
  history, status glyphs, actions and their shortcuts, editor choices, search
  box, settings screen, shortcut list, dialogs, the chat body under a chat's
  header with its shell, the empty state, and the `Frontend` interface the
  binary opens a layout through. It names no layout.
- **ui-classic** — the Classic layout: rail, chat list, chat header, toolbar,
  footer and the workspace that lays them out. Its views read runtime and app
  state and send intents.
- **platform** — Linux adapters: tray, D-Bus notifications, systemd scope,
  desktop motion, single instance and SIGTERM. It calls back into the app or
  UI for window and workspace actions instead of naming them.
- **bin** — composition root: starts the app, builds the runtime, opens the
  layout, wires the scopes, and wires the notifications and tray to the
  layout through `Frontend`.

## Where things belong

| New thing | Layer |
| --- | --- |
| SQL, migration, query | `core::store` |
| Transcript parser or CLI session discovery | `core::transcript`, `core::sessions` |
| Git worktree or process control | `core::worktree`, `core::process` |
| State machine for open/stop/delete races | `core::lifecycle` |
| Use case that touches the database and the UI | `app` |
| Chat, terminal or shell process, open-chat flow, startup or shutdown | `runtime` |
| Button, row, pane, dialog | `ui-kit` when every layout uses it, else the layout's crate |
| Workspace layout, focus, or a dialog's question | `ui-classic::workspace` |
| Tray, D-Bus, systemd | `platform` |

## Rules

1. `core` has no UI dependency.
2. The ui crates never talk to the store, processes or the filesystem; they
   read runtime state and send intents.
3. One owner per piece of state. Catalog data lives in `app`, not in a view.
4. Writes run in the order asked for; reads see the writes asked for before
   them (see `core::store`).
5. The database schema and migration order are append-only.
6. Refactors do not change user-visible behavior, strings, shortcuts or UI.

## Verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-arch.sh
```

UI, chat, PTY, session and resume changes also run the `verify` skill.
