# Working in a chat

When you open a chat, its agent fills the main part of the window. This is a real terminal: you type to the agent, and it answers, exactly as it does when you run it yourself. Every feature and command of the agent works here too.

Agents keep running when you switch to another chat, so you can have several agents working at the same time.

## Copy and paste

In a terminal, **Ctrl+C** is used to interrupt the agent, so copying and pasting use different keys:

- **Copy**: select text with the mouse, then press **Ctrl+Shift+C**.
- **Paste**: **Ctrl+Shift+V**.

## Making the text bigger or smaller

Hold **Ctrl** and scroll with the mouse wheel over a terminal. You can also set the size in [Settings](settings.md), under **Terminal text**.

## Giving the agent files and images

Drag files or images from your file manager and drop them on the chat. Agents Hub types their locations into the terminal for you, and agents that understand images attach them, as if you had pasted them.

## The project shell

Sometimes you want to run a quick command yourself, such as starting your app or checking a file, without leaving the chat. Click **Shell** at the top of the chat, or press **Ctrl+`** (Ctrl and the key above Tab). A terminal opens under the agent, in the same folder the agent works in.

Press the same key again to hide it. The shell keeps running while hidden, and each project has its own.

## Opening the folder in your editor

To look at what the agent changed in your own editor, click the editor's icon at the top of the chat. It opens the folder the agent works in, which for a chat in a worktree is that chat's own copy of the project.

Agents Hub lists the editors it knows that are installed: Zed, VS Code, VSCodium, Cursor, Sublime Text, Emacs and Kate. Click the arrow beside the icon to open the folder in another of them; that one is then used from every chat. You can also choose it in [Settings](settings.md), under **Code editor**. The button only shows when one of these editors is installed.

## Earlier sessions

A chat can go through several conversations with its agent, for example when you clear the conversation and start again with the agent's own commands. Agents Hub keeps a read-only copy of the earlier ones.

When a chat has earlier sessions, a button such as **2 earlier** (with a clock) shows at the top of the chat. Click it, or press **Ctrl+Shift+H**, to show or hide them above the terminal. Click a session to see what was said, including the agent's tool use.

## When an agent stops

If the agent exits, for example because you quit it, a message says so and its last output stays on screen. Click **Restart agent** to start it again in the same conversation.

## Picking up where you left off

When you open a chat that was closed, or start Agents Hub again, the chat's agent resumes the conversation it was in last, so you can carry on as if nothing happened.
