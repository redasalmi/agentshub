# Projects and chats

## The sidebar

The left side of the window has two parts:

- **The rail**, on the far left, lists your projects. Each project shows as a small mark made from its initials. Click a project to select it. A number next to a project counts its chats that are running, and a filled number means some of them need you.
- **The chat list**, next to the rail, shows the chats of the selected project.

To show the projects' full names in the rail instead of only their marks, click the panel button at the bottom of the rail, next to the gear, or search for **Show the projects' names**. You can drag the edge of the sidebar to make it wider or narrower.

### Finding your way around the chat list

The chat list is split into groups:

- **Pinned**: chats you pinned to the top.
- **Chats**: the rest.
- **Archived**: chats you put away. Click the heading to show or hide them.

At the top of the list you can choose what it shows:

- **All**: every chat.
- **Needs you**: chats whose agent is waiting for you, has finished, or stopped on an error.
- **Running**: chats whose agent is running right now.

## What the status dots mean

Each chat has a small dot or symbol in front of it that tells you what its agent is doing. It also has its own shape, so you can tell them apart without relying on colour.

| Status | What it means |
| --- | --- |
| saved | The agent isn't running. Open the chat to pick the conversation up where you left it. |
| running | The agent is running but hasn't said what it is doing. |
| idle | The agent is waiting for your next instruction. |
| working (blue) | The agent is busy on your request. |
| needs permission, has a question, needs you to sign in, waiting for you (amber) | The agent can't go on until you answer. |
| done (green) | The agent has finished, and you haven't looked at the result yet. |
| stopped on an error (red) | The agent failed and stopped. |

## Seeing every running chat at once

Click the **Running** button at the top of the window, or press **Ctrl+0**, to see the running chats of every project in one list, with the ones that need you first.

To jump straight to the next chat that needs you, press **Ctrl+Shift+J**.

## Opening chats quickly

- Hold **Alt** for a moment to see numbers next to the chats. Press **Alt+1** to **Alt+9** to open a chat.
- Hold **Ctrl** for a moment to see numbers next to the projects. Press **Ctrl+1** to **Ctrl+9** to select a project.
- When no chat is open, the **Recent** list shows the chats you used last.

## Managing a chat

Hover a chat in the sidebar to see its buttons, or right-click it for the full menu. The same actions are in the **More** menu at the top of an open chat.

- **Rename**: give the chat a name of your own (or press **F2**). Leave the name empty to go back to the name taken from the agent's conversation.
- **Pin to the top** / **Unpin**: keep important chats at the top of the list.
- **Reorder**: drag a chat and drop it on another chat of the same project.
- **Close**: stop the agent but keep the chat. Open it again later and the agent picks up the same conversation.
- **Archive**: close the chat and move it to the **Archived** group, out of the way (or press **Ctrl+Shift+A**). A message lets you undo it right away. To bring an archived chat back, open it, or choose **Bring back**.
- **Delete**: stop the agent and remove the chat from Agents Hub for good. The agent's own records of the conversation are not deleted.

If the agent is in the middle of something when you close or archive its chat, Agents Hub asks you first, since this stops it mid-task.

### Closing many chats at once

- **Close the idle chats** (**Ctrl+Shift+I**) closes every chat whose agent is resting with nothing you would miss.
- **Close all the chats** (**Ctrl+Shift+W**) closes every open chat.

Closed chats stay in the sidebar, and open again where they left off.

## Managing a project

Right-click a project in the rail, or use the **More** menu at the top of the chat list:

- **New chat…**: start an agent in the project.
- **Archive**: put the project and its chats away. Open one of its chats again, for example from search, to bring the project back.
- **Remove**: stop the project's agents and remove it and its chats from Agents Hub. Your folder and its files are not touched.

To reorder your projects, drag one in the rail and drop it on another.

## The Triage layout

Triage is the other layout, which you can pick under **Look and feel** in the [settings](settings.md). Instead of a rail of projects and a chat list for each, it shows the chats of every project in one list, grouped by what their agents are doing, with the ones that need you first:

- **Needs you**: the agent is waiting for you, stopped on an error, or exited.
- **Ready to review**: the agent has finished, and you haven't looked at the result yet.
- **Working**: the agent is running or busy on your request.
- **Idle**: the agent is waiting for your next instruction.
- **Closed**: the agent isn't running. Click the heading to show or hide them.
- **Archived**: chats you put away. Click the heading to show or hide them.

A chat you open stays in its group while you read it, until its agent does something new, so the list doesn't move under the pointer.

### The project filter

The menu at the top of the list, **All projects** to begin with, picks whose chats the list shows: every project's, or one project's. Choosing doesn't start or stop any agent. Hover a project in the menu to see its own buttons: **New chat…**, **Archive…** and **Remove…**. **Add project…** is at the bottom of the menu. The projects keep the order you gave them in Classic's rail.

### Pinning and the numbers

- Right-click a chat, or use the **More** menu at the top of an open chat, to pin it: a pinned chat comes first in its group. Chats can't be dragged into another order in Triage.
- Hold **Alt** for a moment to see numbers on the chats in the list, and press **Alt+1** to **Alt+9** to open one. Chats in a hidden group have no number.
- Press **Ctrl+1** to **Ctrl+9** to show one project's chats, by its place in the project filter's menu.

### The banner

When the open chat needs you, a banner under its header says what it needs, for example "Claude Code has a question. Answer it in the terminal below." When other chats need you too, its **Next: …** button opens the next one, as **Ctrl+Shift+J** does.
