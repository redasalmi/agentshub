# Closes a Codex chat from the sidebar and types after it.
# Covers: closing a chat hands the keyboard back to the workspace, so keys
# reach neither the closed CLI nor a terminal that is gone.

project="$AHV_WORK/project"
seed_project "$project"

app_start
key ctrl+1
key ctrl+shift+n
key 2
key Return
wait_until 15 "fake codex to start" cli_started codex

codex_exited() { grep -q '^exit codex' "$CLI_LOG"; }

# Right-click the chat's row, then pick Close in its menu.
xdotool mousemove 300 145
sleep 0.3
xdotool click 3
sleep 0.4
click 346 245
wait_until 10 "codex to exit" codex_exited

type_text "x"
key Return
if cli_got_input codex x; then
	fail "codex got a key typed after its chat closed"
fi
if grep -q panicked "$AHV_OUT/app.log"; then
	fail "the app panicked"
fi
shot closed-codex

app_stop
assert_clis_gone
