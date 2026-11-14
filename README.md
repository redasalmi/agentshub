# Agents Hub

A light Linux desktop home for the agent CLIs you already use: [Claude Code](https://code.claude.com), [Codex](https://developers.openai.com/codex) and [Pi](https://github.com/earendil-works/pi). Each chat runs the real CLI in a real terminal, so every feature of the CLI works as it does in your own terminal.

- Projects in a sidebar, each with its chats and a shell of its own.
- A chat can get its own git worktree and branch.
- Chats resume the CLI's last session when you reopen them, and keep their history.
- Search across chats, projects and what was said in them.
- A desktop notification, a sound, or both when an agent finishes or needs you while you look elsewhere.
- A tray icon, to keep it running with the window closed.
- Everything a CLI starts is stopped when its chat closes or the app quits.

It is not an orchestrator: no fleets of agents, task queues or PR review panes.

## Requirements

- Linux on x86_64.
- At least one of `claude`, `codex` or `pi` on your `PATH`.
- A systemd user manager, so that everything a CLI starts can be stopped with it. Without one, only the CLI's own session is stopped.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/redasalmi/agentshub/main/install.sh | sh
```

This installs `~/.local/bin/agentshub` and an app menu entry. The download is checked against its published SHA-256 checksum.

- **Update:** run the same command again.
- **Install a given release:** `curl -fsSL .../install.sh | AGENTSHUB_VERSION=v0.14.0 sh`
- **Uninstall:** `curl -fsSL .../install.sh | sh -s -- --uninstall`. Your data is kept.

Your projects, chats and settings live in `$XDG_DATA_HOME/agentshub/agentshub.db` (by default `~/.local/share/agentshub/`). You can export or import that database from the settings.

## Build from source

You need Rust 1.91 or newer, [Zig 0.15.2](https://ziglang.org/download/) to build Ghostty's terminal library (`mise install` sets it up from `mise.toml`), and the development packages for ALSA, fontconfig, FreeType, Wayland, xkbcommon and XCB. On Debian or Ubuntu:

```sh
sudo apt-get install pkg-config libasound2-dev libfontconfig-dev libfreetype-dev \
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libx11-xcb-dev
```

Then:

```sh
cargo run                    # a dev build
cargo run --profile dev-fast # rebuilds faster, runs slower
cargo build --release
```

Before sending a change:

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

[REVIEW.md](REVIEW.md) lists what a change must not break and how to try a dev build without touching your installed copy.
