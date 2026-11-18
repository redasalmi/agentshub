# For each agent, start a chat, save another config folder for it in the
# settings and start a second chat, then relaunch and check that each chat
# resumed in its own folder. The fake CLIs keep their sessions in their
# config folder as the real ones do, and only resume one they find there.
# Covers: the Agents settings, saving a folder, new chats taking it, Codex
# found through its open rollout and pi through its report in either folder,
# and each CLI's variable set for a chosen folder and unset for the default.

project="$AHV_WORK/project"
agents=(claude codex pi)
seed_project "$project"

default_dir() {
	case $1 in
	claude) echo "$HOME/.claude" ;;
	codex) echo "$HOME/.codex" ;;
	pi) echo "$HOME/.pi/agent" ;;
	esac
}
recorded() {
	[ "$(sql "SELECT COUNT(*) FROM sessions
		WHERE agent = '$1' AND COALESCE(cli_id, agent_id) = '$2'")" = 1 ]
}
saved() { [ "$(sql "SELECT value FROM settings WHERE key = '$1_config_dir'")" = "$HOME/$1-work" ]; }
started() { [ "$(cli_count "$1")" -ge "$2" ]; }
resumed_line() { grep -E "^start $1 .* resume=$2 " "$CLI_LOG" | tail -1; }
resumed() { [ -n "$(resumed_line "$1" "$2")" ]; }

# start_chat INDEX AGENT COUNT DIR VAR: starts the agent from the dialog,
# checks the folder it got, prompts it and waits for its session. Prints the
# session.
start_chat() {
	local line session
	key ctrl+shift+n
	key "$1"
	wait_until 15 "$2 #$3 to start" started "$2" "$3"
	line=$(cli_started "$2")
	session=$(field session "$line")
	assert_eq "$(field config "$line")" "$4" "$2 #$3's folder"
	assert_eq "$(field configvar "$line")" "$5" "$2 #$3's variable"
	# A session without a prompt isn't resumed.
	type_text "prompt to $2 $3"
	key Return
	wait_until 5 "$2 #$3 to get its prompt" cli_got_input "$2" "prompt to $2 $3"
	wait_until 15 "$2 #$3's session in the database" recorded "$2" "$session"
	echo "$session"
}

app_start
key ctrl+1
declare -A first second
for i in 0 1 2; do
	agent=${agents[$i]}
	first[$agent]=$(start_chat $((i + 1)) "$agent" 1 "$(default_dir "$agent")" unset)
done
shot first-chats

key ctrl+comma
# The Agents group, scrolled into view: Claude Code, Codex and Pi fields.
xdotool mousemove 640 400
for _ in 1 2 3 4 5 6 7 8 9 10; do xdotool click 5; sleep 0.1; done
shot agents
fields=(268 406 544)
for i in 0 1 2; do
	agent=${agents[$i]}
	click 922 "${fields[$i]}"
	type_text "~/$agent-work"
	key Return
	wait_until 5 "$agent's folder in the settings" saved "$agent"
	[ -d "$HOME/$agent-work" ] || fail "$HOME/$agent-work wasn't created"
	assert_eq "$(stat -c %a "$HOME/$agent-work")" 700 "$agent's created folder's mode"
done
shot saved
key ctrl+comma

for i in 0 1 2; do
	agent=${agents[$i]}
	second[$agent]=$(start_chat $((i + 1)) "$agent" 2 "$HOME/$agent-work" set)
done
shot second-chats

app_stop
assert_clis_gone

app_start
for n in 1 2 3 4 5 6; do key "alt+$n"; done
for agent in "${agents[@]}"; do
	wait_until 15 "$agent's first chat to resume" resumed "$agent" "${first[$agent]}"
	wait_until 15 "$agent's second chat to resume" resumed "$agent" "${second[$agent]}"
done
shot resumed
if grep -q "^missing" "$CLI_LOG"; then fail "a chat resumed in the wrong folder: $(grep "^missing" "$CLI_LOG")"; fi
for agent in "${agents[@]}"; do
	line=$(resumed_line "$agent" "${first[$agent]}")
	assert_eq "$(field config "$line")" "$(default_dir "$agent")" "$agent's first chat's folder on resume"
	assert_eq "$(field configvar "$line")" unset "$agent's first chat's variable on resume"
	line=$(resumed_line "$agent" "${second[$agent]}")
	assert_eq "$(field config "$line")" "$HOME/$agent-work" "$agent's second chat's folder on resume"
	assert_eq "$(field configvar "$line")" set "$agent's second chat's variable on resume"
done
