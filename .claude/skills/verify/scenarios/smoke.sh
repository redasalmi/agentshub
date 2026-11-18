# Start a Claude chat, type to it, quit, relaunch and resume it.
# Covers: chat start, PTY input, session recording, CLI exit on quit, and
# resume after a restart (see 7afd4c7).

project="$AHV_WORK/project"
seed_project "$project"

app_start
shot project
key ctrl+1
key ctrl+shift+n
shot new-chat
key Return

wait_until 15 "fake claude to start" cli_started claude
started=$(cli_started claude)
session=$(field session "$started")
log "claude started: $started"
assert_eq "$(field cwd "$started")" "$project" "claude's working directory"

type_text "hello from verify"
key Return
wait_until 5 "claude to get the typed line" cli_got_input claude "hello from verify"
assert_eq "$(grep -m1 '^input claude' "$CLI_LOG")" "input claude hello from verify" \
	"claude's first input (a key that started the chat must not reach it)"
shot chat

# Predicates, so each poll queries again.
session_recorded() {
	[ "$(sql "SELECT COUNT(*) FROM sessions WHERE agent = 'claude' AND agent_id = '$session'")" = 1 ]
}
claude_restarted() { [ "$(cli_count claude)" -ge 2 ]; }

wait_until 5 "the session in the database" session_recorded
assert_eq "$(sql "SELECT COUNT(*) FROM chats")" 1 "chats in the database"

app_stop
assert_clis_gone

app_start
shot relaunched
if [ "$(cli_count claude)" -lt 2 ]; then
	key alt+1
fi
wait_until 15 "claude to be resumed" claude_restarted
resumed=$(cli_started claude)
log "claude resumed: $resumed"
assert_eq "$(field resume "$resumed")" "$session" "resumed session"
shot resumed

app_stop
assert_clis_gone
