# Separate checkouts for each chat (git worktrees)

*This page is only useful for projects that use [git](https://git-scm.com).*

If two agents work in the same folder at the same time, they can trip over each other: one changes a file while the other is in the middle of editing it. Agents Hub can avoid this by giving a chat its own copy of the project to work in, on a branch of its own. In git, such a copy is called a **worktree**.

## Starting a chat in its own worktree

When you start a chat in a project that uses git, the **Start a coding agent** window has a box called **Start in a new git worktree**. Tick it, and the agent works in its own copy of the project. Other chats don't touch it.

To have this box ticked for every new chat, turn on **Start chats in a git worktree** in [Settings](settings.md). You can still untick it for a single chat.

## Where your work ends up

- The agent's changes are on a new branch whose name starts with `agentshub/`. You can look at that branch, merge it or throw it away with your usual git tools, as with any other branch.
- The copies are kept in Agents Hub's own data folder (by default `~/.local/share/agentshub/worktrees`), not inside your project.
- The project shell (**Ctrl+`**) of such a chat opens in the chat's copy, so you can run and test what the agent made.

## When you delete the chat

Deleting the chat also removes its copy of the project, with two safety rules:

- If the copy has changes that nobody committed, Agents Hub keeps it and tells you where it is, so nothing is lost.
- The branch always stays, so committed work is never lost.
