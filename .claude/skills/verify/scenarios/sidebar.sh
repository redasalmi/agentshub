# Rename a chat, close the idle chats, quit and reopen it, resuming the
# same session.
# Covers: rename dialog, idle close, chat reopen and session resume after a
# state change.
project="$AHV_WORK/project"
seed_project "$project"

app_start
key ctrl+1
key ctrl+shift+n
key Return

wait_until 15 "fake claude to start" cli_started claude
started=$(cli_started claude)
session=$(field session "$started")
type_text "prompt before renaming"
key Return
wait_until 5 "claude to get the prompt" cli_got_input claude "prompt before renaming"

recorded() {
	[ "$(sql "SELECT COUNT(*) FROM sessions WHERE agent = 'claude' AND agent_id = '$session'")" = 1 ]
}
renamed() {
	[ "$(sql "SELECT COUNT(*) FROM chats WHERE title LIKE '%renamed chat%'")" = 1 ]
}
exited() { grep -q '^exit claude' "$CLI_LOG"; }
cli_count_at_least_two() { [ "$(cli_count claude)" -ge 2 ]; }

wait_until 15 "the session in the database" recorded

key F2
type_text "renamed chat"
key Return
wait_until 5 "the chat's new title" renamed
shot renamed

key ctrl+shift+w
wait_until 10 "claude to exit after closing all chats" exited
shot closed-all

app_stop
assert_clis_gone

app_start
shot relaunched
if [ "$(cli_count claude)" -lt 2 ]; then
	key alt+1
fi
wait_until 15 "claude to start again" cli_count_at_least_two
resumed=$(cli_started claude)
log "claude resumed: $resumed"
assert_eq "$(field resume "$resumed")" "$session" "resumed session"
shot resumed

app_stop
assert_clis_gone
