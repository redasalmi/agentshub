# Opens the Triage layout from the layout setting and drives it: starts a
# chat in the second project from the dialog's Project field, filters the
# list by project, opens chats by their card's number and from the search,
# pins, archives and brings back a chat, restarts an agent that exited, then
# relaunches.
# Covers: the layout setting opens Triage, the dialog starts in the project
# picked in it, filtering only changes what is listed, Alt numbers and the
# search show and focus a chat, pinning and archiving with Undo, the error
# notice and its Restart under Triage's header, and relaunching into Triage.

first="$AHV_WORK/first"
second="$AHV_WORK/second"
# A project seeded later comes first.
seed_project "$second"
seed_project "$first"
sqlite3 "$DB" "INSERT INTO settings (key, value) VALUES ('layout', 'triage')
	ON CONFLICT (key) DO UPDATE SET value = excluded.value;"

counts_at_least() { [ "$(cli_count claude)" -ge "$1" ] && [ "$(cli_count codex)" -ge "$2" ]; }

app_start
shot opened

# Start: the dialog opens for the first project; pick the second in its
# Project field, name the chat and start Claude with Enter.
key ctrl+shift+n
click 640 171
click 460 244
click 640 295
type_text "Fix the login redirect"
key Return
wait_until 15 "claude to start" cli_started claude
assert_eq "$(field cwd "$(cli_started claude)")" "$second" "the new chat's folder"
shot started

# Filter keeps chats running: a Codex chat in the first project, then the
# list filtered to each project and back to all through the filter's menu.
key ctrl+shift+n
click 640 171
click 460 213
click 640 295
type_text "Write the release notes"
click 640 234
wait_until 15 "codex to start" cli_started codex
assert_eq "$(field cwd "$(cli_started codex)")" "$first" "the Codex chat's folder"
claudes=$(cli_count claude)
codexes=$(cli_count codex)
key ctrl+1
key ctrl+2
shot filtered
click 100 26
click 100 64
assert_eq "$(cli_count claude)" "$claudes" "the Claude CLIs started across filters"
assert_eq "$(cli_count codex)" "$codexes" "the Codex CLIs started across filters"
grep -q '^exit ' "$CLI_LOG" && fail "a CLI exited while the filter changed"
shot all-projects

# Alt+1: show the second card, then Alt+1 shows and focuses the first, the
# newest chat.
click 170 260
key alt+1
shot alt-one
type_text "from alt one"
key Return
wait_until 5 "codex to get the line typed after Alt+1" cli_got_input codex "from alt one"

# Search opens the other chat, which then has the keyboard.
key ctrl+shift+k
type_text "login redirect"
key Return
shot searched
type_text "from the search"
key Return
wait_until 5 "claude to get the line typed after the search" cli_got_input claude "from the search"

# Pin the first card's chat from its menu, which keeps it first, show it
# with Alt+1, archive it with Ctrl+Shift+A and take that back with Undo.
xdotool mousemove 170 160
sleep 0.3
xdotool click 3
sleep 0.4
shot card-menu
click 240 232
pinned() { [ -n "$(sql "SELECT id FROM chats WHERE pinned = 1")" ]; }
wait_until 5 "a chat to be pinned" pinned
chat=$(sql "SELECT id FROM chats WHERE pinned = 1")
archived() { [ -n "$(sql "SELECT archived_at FROM chats WHERE id = $chat")" ]; }
listed() { ! archived; }
key alt+1
key ctrl+shift+a
wait_until 5 "the pinned chat to be archived" archived
shot archived
click 1205 76
wait_until 5 "Undo to bring the chat back" listed
shot undone

# An agent that exits shows the error notice under the header; Restart
# resumes its session.
other=$(sql "SELECT agent || '|' || title FROM chats WHERE id != $chat")
agent=${other%%|*}
key ctrl+shift+k
type_text "${other#*|}"
key Return
started=$(cli_started "$agent")
before=$(cli_count "$agent")
kill "$(field pid "$started")"
exited() { grep -q "^exit $agent pid=$(field pid "$started")$" "$CLI_LOG"; }
wait_until 10 "$agent to exit" exited
shot exited
# Its card leads Needs you, which isn't folded, so Alt+1 shows it again from
# the settings.
key ctrl+comma
key alt+1
shot exited-needs-you
click 1220 78
restarted() { [ "$(cli_count "$agent")" -gt "$before" ]; }
wait_until 15 "$agent to restart" restarted
assert_eq "$(field resume "$(cli_started "$agent")")" "$(field session "$started")" "the session $agent resumed"
shot restarted

if grep -q panicked "$AHV_OUT/app.log"; then
	fail "the app panicked"
fi

# Relaunch: no CLI is left, and the window opens in Triage again.
app_stop
assert_clis_gone
app_start
shot relaunched
app_stop
