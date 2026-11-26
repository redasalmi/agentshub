# Separate checkouts for each chat (git worktrees)

*This page is only useful for projects that use [git](https://git-scm.com).*

If two agents work in the same folder at the same time, they can trip over each other: one changes a file while the other is in the middle of editing it. Agents Hub can avoid this by giving a chat its own copy of the project to work in, on a branch of its own. In git, such a copy is called a **worktree**.

## Starting a chat in its own worktree

When you start a chat in a project that uses git, the **Start a coding agent** window has a box called **Start in a new git worktree**. Tick it, and the agent works in its own copy of the project. Other chats don't touch it.

To have this box ticked for every new chat, turn on **Start chats in a git worktree** in [Settings](settings.md). You can still untick it for a single chat.

## What the copy holds

The copy starts as your project folder is at that moment, file for file: changes you haven't committed, new files, and the files git ignores, such as `.env`, `node_modules` or a build folder. The agent can run your tests and tools there straight away, without installing anything or asking you for settings.

- The new branch starts from the commit you have checked out. Your uncommitted changes are in the copy as uncommitted changes, including ones you had staged.
- On btrfs or XFS, the copy shares the files' contents with your project until one side changes them, so even a large `node_modules` takes almost no extra room. On other filesystems, everything is copied in full.
- Python virtual environments (a `.venv` or any folder with a `pyvenv.cfg`) aren't copied, since they point back to your project folder and would run its code instead of the copy's. The agent makes its own, as with `uv sync`.
- Files and folders you can't read, such as a database folder a container owns, are left out, and Agents Hub tells you which.
- Submodules are left empty, as git leaves them in any new worktree. To have them in the copy, run `git submodule update --init` there.

## Where your work ends up

- The agent's changes are on a new branch whose name starts with `agentshub/`. You can look at that branch, merge it or throw it away with your usual git tools, as with any other branch.
- The copies are kept in Agents Hub's own data folder (by default `~/.local/share/agentshub/worktrees`), not inside your project.
- The project shell (**Ctrl+`**) of such a chat opens in the chat's copy, so you can run and test what the agent made.

## When you delete the chat

Deleting the chat also removes its copy of the project, with two safety rules:

- If the copy has changes that nobody committed, Agents Hub keeps it and tells you where it is, so nothing is lost. That includes the uncommitted changes it was copied with. Ignored files, like `node_modules`, don't count.
- The branch always stays, so committed work is never lost.
