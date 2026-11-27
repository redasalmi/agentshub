# Closes a Codex chat from the sidebar and starts another chat from the
# keyboard, pins, archives and brings back that chat, then quits while a new
# chat's worktree is checked out.
# Covers: closing a chat hands the keyboard back to the workspace, pinning,
# archiving with Undo and bringing back an archived chat, and quitting during
# a checkout leaves no CLI and no worktree behind.

project="$AHV_WORK/project"
seed_project "$project"

app_start
key ctrl+1
key ctrl+shift+n
key 2
key Return
wait_until 15 "fake codex to start" cli_started codex

codex_exited() { grep -q 'exit codex pid=' "$CLI_LOG"; }

# Right-click the chat's row, then pick Close in its menu.
xdotool mousemove 300 145
sleep 0.3
xdotool click 3
sleep 0.4
click 346 245
wait_until 10 "codex to exit" codex_exited

# Only the workspace's shortcut opens the dialog, whose number key starts
# Claude, so the keyboard came back to the workspace.
key ctrl+shift+n
shot new-chat-after-close
key 1
wait_until 15 "fake claude to start from the keyboard" cli_started claude
if grep -q panicked "$AHV_OUT/app.log"; then
	fail "the app panicked"
fi
shot claude-after-close

# Pin the Claude chat from the search box, archive it with its shortcut and
# take that back with the notification's Undo, then archive it again and
# bring it back from the sidebar's archived chats.
chat=$(sql "SELECT id FROM chats WHERE agent = 'claude'")
pinned() { [ "$(sql "SELECT pinned FROM chats WHERE id = $chat")" = 1 ]; }
archived() { [ -n "$(sql "SELECT archived_at FROM chats WHERE id = $chat")" ]; }
listed() { ! archived; }
claude_exits() { [ "$(grep -c 'exit claude pid=' "$CLI_LOG")" -ge "$1" ]; }
claudes_at_least() { [ "$(cli_count claude)" -ge "$1" ]; }

key ctrl+shift+k
type_text "pin this chat"
key Return
wait_until 5 "the chat to be pinned" pinned
shot pinned

key ctrl+shift+a
wait_until 5 "the chat to be archived" archived
wait_until 10 "claude to stop with its archived chat" claude_exits 1
shot archived
click 1205 76
wait_until 5 "Undo to bring the chat back" listed
shot undone
pinned || fail "the chat lost its pin coming back"

key alt+1
wait_until 15 "claude to start again in its chat" claudes_at_least 2
key ctrl+shift+a
wait_until 5 "the chat to be archived again" archived
wait_until 10 "claude to stop again" claude_exits 2
shot archived-again

# Show the archived chats, right-click the chat's row, then pick Bring back
# in its menu.
click 215 195
xdotool mousemove 300 240
sleep 0.3
xdotool click 3
sleep 0.4
click 363 285
wait_until 5 "Bring back to restore the chat" listed
shot restored

app_stop
assert_clis_gone

# Quit during checkout: the untracked file makes copying the project take a
# moment.
big="$AHV_WORK/big"
seed_project "$big"
git -C "$big" -c user.name=verify -c user.email=verify@example.com \
	commit -q --allow-empty -m init
head -c 20000000 /dev/urandom >"$big/big.bin"
claudes=$(cli_count claude)

app_start
key ctrl+1
key ctrl+shift+n
key Tab
key Tab
key space
shot worktree-ticked
xdotool key --clearmodifiers Return
app_stop
assert_clis_gone
[ -d "$XDG_DATA_HOME/agentshub/worktrees/big" ] || fail "no checkout started"
assert_eq "$(cli_count claude)" "$claudes" "the Claude chats started"
assert_eq "$(git -C "$big" worktree list | wc -l)" 1 "the worktrees left after quitting during a checkout"
