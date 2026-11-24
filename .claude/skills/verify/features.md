# Feature map

What a user can do and what should happen, to pick a scenario's steps and
judge its screenshots. How to do each one (keys, clicks) is in `docs/`, mostly
`docs/keyboard-shortcuts.md` and `docs/getting-started.md`; don't repeat it here.

| Feature | Expected | Scenario |
| --- | --- | --- |
| Empty start | "Add a project to begin" | |
| Add project | Project in the rail, selected | seeded instead (no file picker) |
| Select project | Its chats in the sidebar; a button per installed CLI | smoke |
| Start from the empty state | That agent starts in the project folder | |
| New chat dialog | Agent cards, name field, worktree checkbox in git projects | smoke |
| Start from the dialog | Enter starts the picked agent, a number or click starts that one; the chat runs it | smoke |
| Chat in a worktree | CLI's cwd is a new checkout on its own branch | |
| Type to the agent | The CLI gets the keys; nothing else does | smoke |
| Chat title | Sidebar and header show the first prompt or the CLI's title | smoke |
| Resume after restart | The chat resumes the same session (`--resume <id>`) | smoke |
| Quit | No CLI left running | smoke |
| Open chat by place, running chats, next chat needing you | That chat or list opens | |
| Search | Finds chats, messages, projects, commands | |
| Settings, shortcut list | Open and close | |
| Agent config folder | Saving a folder creates it; new chats of that agent run in it, older ones resume in theirs | config-folder |
| Rename chat | Title editable and saved | |
| Archive, close idle, close all | Those chats leave; their CLIs stop | |
| Project shell | Shell in the project shows and hides | |
| Earlier sessions | The chat's earlier sessions show and hide | |
| Copy, paste | Selection copied; clipboard pasted to the CLI | |
| Font size | Terminal text grows and shrinks | rendering |
| Agent output | Box lines, blocks and braille meet without seams; check marks, symbols and emoji draw (emoji in one color where the system's color emoji are COLRv1) | rendering |
