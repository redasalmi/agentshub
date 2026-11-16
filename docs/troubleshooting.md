# Questions and troubleshooting

### It says "No coding agent is installed"

Agents Hub starts the agents you already have installed. Install Claude Code, Codex or Pi with their own install guide, then check that typing `claude`, `codex` or `pi` in a terminal starts it. Agents Hub notices a newly installed agent without a restart.

### An agent asks me to sign in

Answer it in the chat, as you would in your terminal. Agents Hub doesn't handle your accounts: each agent uses its own sign-in.

### A chat shows "running" but never "working" or "done"

The status comes from what the agent itself reports. When an agent doesn't say what it is doing, Agents Hub can only tell that it is running.

### I closed the window and my agents stopped

Turn on **Keep running in the tray** in [Settings](settings.md). If your desktop has no tray, closing the window always quits the app.

### I don't get notifications

Check that **Show a notification** or **Play a sound** is on in [Settings](settings.md), and click **Send a test**. Notifications only appear while you're looking elsewhere, not for the chat on screen. Also check that your desktop's do-not-disturb mode is off.

### Ctrl+C doesn't copy

In a terminal, Ctrl+C interrupts the agent. Use **Ctrl+Shift+C** to copy and **Ctrl+Shift+V** to paste.

### Does deleting a chat or removing a project delete my files?

No. Your project folder and its files are never touched. Deleting a chat only removes it from Agents Hub, and the agent's own records of the conversation stay. The one exception is a chat's separate copy of the project, if it had one: see [Separate checkouts for each chat](worktrees.md).

### Where are my chats kept?

In one file on your computer, by default in `~/.local/share/agentshub/`. Use **Export a copy** in [Settings](settings.md) to back it up. Uninstalling Agents Hub keeps it.

### "Agents Hub can't start"

Agents Hub couldn't open its data file, or another copy of Agents Hub is already using it. The message says which. Quit any other copy that is running and open Agents Hub again.
