# Review rules

Failure here looks like:

- session history in SQLite lost or corrupted (migrations must be additive and tested)
- a CLI's PTY killed, orphaned, or leaking processes
- a git command run on the wrong worktree or branch
- the UI blocking on I/O on the foreground thread
- notifications that fire wrongly or not at all
- a workaround where the CLI has a native interface, or a feature outside the non-goals in CLAUDE.md

Driving the app:

Drive the dev build inside a hidden compositor of its own, never on the developer's desktop. Its input and screenshots stay in that session, so the developer keeps their mouse and keyboard and nothing of theirs gets clicked, typed into or captured.

- Isolate it from the installed copy's single-instance lock, IPC name and database: its own session bus (`dbus-run-session --`), a short `XDG_RUNTIME_DIR` (Wayland socket paths must be under 108 bytes) and its own `XDG_DATA_HOME`. Put a fake CLI first on a trimmed `PATH` so no real agent starts.
- On KDE, start a headless KWin with Xwayland and run the test script inside it. KWin sets `DISPLAY` for the script. Give it its own `XDG_CONFIG_HOME` whose `kwinrc` turns off the input-control prompt, and never edit the developer's own `kwinrc`:

  ```sh
  printf '[Xwayland]\nXwaylandEisNoPrompt=true\n' > "$CFG/kwinrc"
  env -i HOME="$HOME" PATH="$FAKE_BIN:/usr/bin:/bin" XDG_RUNTIME_DIR="$RT" XDG_CONFIG_HOME="$CFG" \
    dbus-run-session -- kwin_wayland --virtual --no-lockscreen --width 1280 --height 800 \
      --socket hubtest --xwayland --exit-with-session "bash drive.sh"
  ```

- In the script, launch the app on Xwayland with `env -u WAYLAND_DISPLAY XDG_DATA_HOME=...`, find it with `xdotool search --class agentshub`, and drive it with `xdotool`. Pause about 0.3s between `mousemove` and `click`, or the click is lost. Screenshot with `spectacle -b -n -f -o shot.png`. A full-screen shot is fine here because the screen is the virtual one.
- If the hidden session can't run, ask the developer before driving their real desktop.
- Stop the test copy by its process ID or full binary path, never by process name, which also matches the installed copy. `--exit-with-session` stops KWin when the script ends. Then check that no test processes are left.

Rules:

- Cite file:line for every finding. Never guess from a name.
- At most 3 nits.
- Tag bugs the change didn't introduce as pre-existing.
- Skip `target/`, `Cargo.lock`, `assets/`.
- Start the summary with the finding count. Say "nothing to fix" plainly when clean.
