---
name: verify
description: Run the agentshub dev build in a hidden KWin session with fake agent CLIs, drive it with xdotool, and check the result through screenshots, the fake CLIs' log and the app's database. Use before saying a UI, chat, PTY, session or resume change is done, or when asked to run, screenshot or smoke-test the app.
---

# Verify a change in the running app

Everything runs in a throwaway session: a headless KWin with Xwayland on its
own D-Bus session bus, with its own `HOME`, `XDG_*` dirs and database, and fake
`claude`, `codex` and `pi` first on `PATH`. The installed copy, the real
CLIs, the desktop and the developer's input aren't touched.

## Run

```sh
cargo build
.claude/skills/verify/scripts/run.sh .claude/skills/verify/scenarios/smoke.sh
```

It prints `PASS` or `FAIL: <why>` and the artifacts folder, by default
`target/verify/<time>-<scenario>/`:

- `NN-<name>.png`: screenshots, in order. On failure, the last one is `fail`.
  **Look at them.** A passing script doesn't prove the screen looks right.
- `run.log`: the scenario's steps. `fake-cli.log`: each CLI start (pid,
  session, resume id, cwd, args), each line typed to it, each exit.
- `app.log`: the app's stderr. `kwin.log`: the compositor's and bus's output.

Options: `AGENTSHUB_BIN` (another binary), `AHV_TIMEOUT` (seconds, default
300), `AHV_KEEP=1` (keep the temp dir, including the database), and a second
argument for the output folder, which must be new or empty.

After the run, `run.sh` fails if anything it started is still running and
kills it. It only looks at what the run started: the apps by the PIDs
`app_start` and `app_relaunched` recorded, while each is still the binary
under test, the fake CLIs by the run's own temp dir, and the bus, KWin and
Xwayland by that temp dir in their environment. It never matches by process
name or binary path, which would also match the installed copy or another
run.

## Scenarios

`scenarios/smoke.sh`: seed a project, start a Claude chat from the dialog, type
a line, check the session in the database, quit, check the CLI exited,
relaunch and check the chat resumed the same session.

`scenarios/config-folder.sh`: for Claude, Codex and pi, start a chat, save
another config folder in the settings, start a second chat, relaunch and check
each chat resumed in its own folder, with the CLI's variable unset for the
default one.

`scenarios/worktree.sh`: seed a project with uncommitted changes, an
untracked file, `.env`, `node_modules`, a virtualenv and an unreadable
folder, start a chat in a worktree, check the notice about the unreadable
folder, check
the CLI runs in it and that it matches the project path for path (kind, mode,
size, modification time, link target, contents and git status), and run
commands in its project shell.

`scenarios/runtime.sh`: start a Codex chat, close it from its row's menu, and
check the keyboard is back with the workspace by starting Claude from the New
Chat dialog; pin that chat from the search box, archive it with Ctrl+Shift+A
and take it back with Undo, archive it again and bring it back from the
sidebar's archived chats; open the shortcut list, the settings and the running
chats from the keyboard; then start a chat in a worktree of a project with a
large untracked file, quit while it is checked out, and check no CLI and no
worktree is left.

`scenarios/rendering.sh`: has the fake Claude draw what fonts alone get wrong
(its block logo, check marks, boxes, a usage bar, braille, Powerline, emoji and
text attributes) and shoots it at the default size and zoomed in. Compare the
shots with the same text in Ghostty.

`scenarios/editor.sh`: installs a fake Zed (with an icon) and VS Code (without),
opens the chat's folder from the header, picks VS Code in its menu and checks it
only became the default, opens the folder again from the icon and with Ctrl+Shift+E, checks the
launches in `editor.log`, the environment handed back to the editor and the
saved setting, then picks Zed in the settings and checks nothing opened.
Editors installed under `/usr/share` show in the menus too, after the fakes.

`scenarios/triage.sh`: saves the Triage layout and opens it, starts Claude in
the second project from the New Chat dialog's Project field and Codex in the
first, filters the list to each project and back to all and checks no CLI
started or stopped, opens chats with Alt+1 and the search and types to them,
pins a chat from its card's menu, archives it with Ctrl+Shift+A and takes it
back with Undo, kills an agent, checks Alt+1 finds its card under Needs you
and restarts it from the notice under the header, checking it resumed its
session, then quits and relaunches into
Triage.

`scenarios/layout.sh`: with a Claude chat running in Classic, picks Triage in
the settings and chooses Later, checks the saved layout, that the window is
still Classic and the agent still runs, and picks Classic again; then moves
and resizes the window, restarts into Triage from the dialog and checks the
agent stopped, the window came back where it was, the saved bounds were
cleared, and the chat resumes its session when opened; an ordinary launch
then opens centred, and a maximized window restarted into Classic from the
setting's note comes back maximized.

To cover a change, add a scenario next to it or a step to an existing one. A
scenario is bash sourced with `set -e` after `scripts/lib.sh`:

| Helper | Does |
| --- | --- |
| `seed_project DIR` | Creates a git repo and adds it as a project (app stopped) |
| `seed_editor ID [SVG]` | Installs a fake editor as desktop entry `ID`, logging `ID\|cwd\|args` to `$EDITOR_LOG` |
| `app_start` / `app_stop` | Launches on Xwayland and waits for the window / SIGTERMs by PID |
| `app_relaunched` | Once the app restarted itself and the first copy exited, waits for the new copy's window and records its PID |
| `key ctrl+shift+n` | xdotool key names; pauses after |
| `type_text "..."` | Types into the focused window |
| `click X Y` | Moves, pauses 0.3s, clicks (the screen is 1280x800) |
| `shot NAME` | Screenshot of the virtual screen |
| `sql "SELECT ..."` | Read-only query of the test database |
| `wait_until SECS "what" CMD...` | Polls; fails with "what" on timeout |
| `cli_started AGENT`, `cli_count AGENT`, `field NAME "$line"` | Read the fake CLI log |
| `cli_got_input AGENT TEXT` | Whether the CLI got that line |
| `assert_clis_gone` | Every fake CLI that started has exited |
| `assert_eq ACTUAL EXPECTED "what"` / `fail "why"` | Fail with a screenshot |

Wait on a condition, never a bare sleep. Give `wait_until` a command or a
function that checks again each time: `test "$(sql ...)" = 1` runs the query
once, before the wait starts. Take keys from
`docs/keyboard-shortcuts.md` and expected results from `features.md`; prefer
shortcuts to clicks, since coordinates move with the layout.

The fake CLI (`scripts/fake-cli`) reports its session the way the real one
does (Claude through the `--settings` SessionStart hook, pi through
`--extension`, Codex not at all), honours `--resume`, `resume` and
`--session`, and echoes each line back, writing it to its transcript. Given
`AHV_SCREEN`, a file, it prints that file first, as a CLI's screen, and logs `screen`.
It keeps transcripts in its config folder (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`PI_CODING_AGENT_DIR` or the default), laid out as each CLI does: Claude's
under `projects/`, Codex's rollout under `sessions/`, held open, with its
`session_meta` line, and pi's under `sessions/` with its header. It logs the
folder as `config=` and whether the variable was set as `configvar=`, and
exits, logging `missing`, when the session to resume isn't in that folder.

## What it can't cover

- systemd scopes: the private bus has no `org.freedesktop.systemd1`, so CLIs
  run without a scope (the fallback). `app.log` says so; that's expected.
- Portals (file picker, reduced motion), the tray and desktop notifications:
  there is no portal, tray host or notification server in the session.
- Real CLI behaviour: the fakes only follow the parts the app relies on.

If KWin can't start here, say so and ask before driving the real desktop.

## Failing

Read `run.log`, the `fail` screenshot and `app.log` before changing anything.
A scenario that fails three times in a row after fixes: stop and ask. Don't add
sleeps or retries to make a flaky step pass; find what it should wait on.
