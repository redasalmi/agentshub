# Getting started

## What you need

- A computer running Linux (64-bit Intel or AMD).
- At least one coding agent installed: [Claude Code](https://code.claude.com), [Codex](https://developers.openai.com/codex) or [Pi](https://github.com/earendil-works/pi). Follow the agent's own install guide, and make sure you can start it by typing `claude`, `codex` or `pi` in a terminal.

## Install Agents Hub

Open a terminal, paste this line and press Enter:

```sh
curl -fsSL https://raw.githubusercontent.com/redasalmi/agentshub/main/install.sh | sh
```

Agents Hub then shows up in your app menu. Open it from there like any other app.

- **To update**, run the same line again.
- **To uninstall**, run:

  ```sh
  curl -fsSL https://raw.githubusercontent.com/redasalmi/agentshub/main/install.sh | sh -s -- --uninstall
  ```

  Your projects, chats and settings are kept, so they are there again if you reinstall.

## Add your first project

A project is a folder you work in, such as the folder of a website or an app you are building.

The first time you open Agents Hub, it says **Add a project to begin**. To add one, do any of these:

- Click **Add project** and choose the folder.
- Drag the folder from your file manager and drop it on the Agents Hub window.
- Press **Ctrl+Shift+O**.

The project appears in the narrow column on the far left of the window, called the **rail**.

## Start your first chat

With the project selected, Agents Hub shows a button for each agent installed on your computer. Click one, or press its number (**1**, **2** or **3**), and the agent starts in that project's folder.

For more choices, click **More options…** or **New chat**, or press **Ctrl+Shift+N**. This opens **Start a coding agent**, where you can:

- pick the agent, by clicking it, pressing its number or using the **←** and **→** arrow keys;
- give the chat a name (you can leave it empty, and the chat is then named after what you talk about with the agent);
- choose whether the agent gets a separate copy of the project to work in, if the project uses git (see [Separate checkouts for each chat](worktrees.md)).

Press **Enter** to start.

The agent now runs in the main part of the window. Type to it as you would in a terminal. The chat also appears in the sidebar, under the project.

## Next steps

- Start a second chat while the first agent works. Agents keep running while you look at another chat.
- Press **Ctrl+Shift+K** to search across everything.
- Press **Ctrl+Shift+/** to see every keyboard shortcut.
