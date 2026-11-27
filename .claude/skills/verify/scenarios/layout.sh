# Switches the layout from the settings: Later keeps the window and its agent
# as they are and saves the layout for the next start, picking the open one
# again takes that back, Restart now reopens in the other layout where the
# window was and the chat resumes once opened, an ordinary launch opens
# centred again, and a maximized window comes back maximized.
# Covers: the Layout row and its note, the restart dialog's Later and Restart
# now, the note's Restart now, the window's bounds kept for one start only,
# and resuming a chat after a layout restart.

project="$AHV_WORK/project"
seed_project "$project"

setting() { sql "SELECT value FROM settings WHERE key = '$1'"; }

# The window as it shows, "X Y WIDTH HEIGHT": without the margin a window
# that isn't maximized keeps around it for its shadow, which its frame
# extents give.
visible() {
	local WINDOW X Y WIDTH HEIGHT SCREEN l r t b
	eval "$(xdotool getwindowgeometry --shell "$WIN")"
	read -r l r t b < <(xprop -id "$WIN" _GTK_FRAME_EXTENTS | sed -n 's/.*= //p' | tr -d ,)
	l=${l:-0} r=${r:-0} t=${t:-0} b=${b:-0}
	echo "$((X + l)) $((Y + t)) $((WIDTH - l - r)) $((HEIGHT - t - b))"
}

# click_in X Y: clicks X,Y from the visible window's top left corner, since
# what is in it moves with the window.
click_in() {
	local at
	read -r -a at < <(visible)
	click $((at[0] + $1)) $((at[1] + $2))
}

# near "X Y W H" "X Y W H": each within a few pixels.
near() {
	local a b i
	read -r -a a <<<"$1"
	read -r -a b <<<"$2"
	for i in 0 1 2 3; do
		[ $((a[i] - b[i])) -le 3 ] && [ $((b[i] - a[i])) -le 3 ] || return 1
	done
}

unmaximized() { [[ $(xprop -id "$WIN" _NET_WM_STATE) != *MAXIMIZED* ]]; }
shrunk() { [ "$(visible)" != "0 0 1280 800" ]; }

maximized() {
	local state
	state=$(xprop -id "$WIN" _NET_WM_STATE)
	[[ $state == *_NET_WM_STATE_MAXIMIZED_VERT* && $state == *_NET_WM_STATE_MAXIMIZED_HORZ* ]]
}

exited() { ! kill -0 "$1" 2>/dev/null; }
bounds_cleared() { [ -z "$(setting restart_bounds)" ]; }

# Later: a Claude chat runs in Classic; picking Triage asks, naming the one
# agent a restart would stop. Later saves Triage for the next start and
# leaves the window in Classic with its agent running.
app_start
key ctrl+1
key ctrl+shift+n
key Return
wait_until 15 "claude to start" cli_started claude
started=$(cli_started claude)
session=$(field session "$started")
type_text "fix the login redirect"
key Return
session_recorded() {
	[ "$(sql "SELECT COUNT(*) FROM sessions WHERE agent = 'claude' AND agent_id = '$session'")" = 1 ]
}
wait_until 5 "the session in the database" session_recorded

key ctrl+comma
click_in 1071 350
shot dialog
click_in 711 200
wait_until 5 "the layout saved as triage" [ "$(setting layout)" = triage ]
shot later
kill -0 "$(field pid "$started")" || fail "claude stopped after Later"
grep -q '^exit ' "$CLI_LOG" && fail "a CLI exited after Later"

# Back: picking Classic, the open layout, drops the note without asking.
click_in 994 350
wait_until 5 "the layout saved as classic" [ "$(setting layout)" = classic ]
shot back

# Restart: with the window moved and resized, Restart now in the dialog
# stops the agent and reopens in Triage where the window was.
xdotool windowstate --remove MAXIMIZED_VERT "$WIN"
xdotool windowstate --remove MAXIMIZED_HORZ "$WIN"
wait_until 5 "the window to be unmaximized" unmaximized
xdotool windowsize "$WIN" 1200 760
xdotool windowmove "$WIN" 40 20
wait_until 5 "the window to move" shrunk
sleep 0.5
before=$(visible)
log "window before the restart: $before"
click_in 1071 350
shot restart-dialog
old=$APP_PID
key Return
wait_until 20 "the first app to exit" exited "$old"
assert_clis_gone
app_relaunched
wait_until 5 "the saved bounds to be cleared" bounds_cleared
after=$(visible)
log "window after the restart: $after"
near "$after" "$before" || fail "the window reopened at $after, not $before"
shot restarted

# Resume: opening the chat resumes its session. Closed chats are folded in
# Triage, out of Alt+1's reach, so the search opens it.
key ctrl+shift+k
type_text "login redirect"
key Return
claude_resumed() { [ "$(cli_count claude)" -ge 2 ]; }
wait_until 15 "claude to be resumed" claude_resumed
assert_eq "$(field resume "$(cli_started claude)")" "$session" "the session claude resumed"
shot resumed

# An ordinary launch opens centred at the default size, in Triage.
app_stop
assert_clis_gone
app_start
assert_eq "$(visible)" "0 0 1280 800" "the window after an ordinary launch"
assert_eq "$(setting layout)" triage "the layout after an ordinary launch"
shot ordinary

# Maximized: a window maximized from a smaller size comes back maximized
# after Later and the note's Restart now, in Classic. With no agent running,
# the dialog doesn't mention one.
xdotool windowstate --remove MAXIMIZED_VERT "$WIN"
xdotool windowstate --remove MAXIMIZED_HORZ "$WIN"
xdotool windowsize "$WIN" 1000 640
xdotool windowmove "$WIN" 60 40
wait_until 5 "the window to be unmaximized" unmaximized
wait_until 5 "the window to shrink" shrunk
xdotool windowstate --add MAXIMIZED_VERT "$WIN"
xdotool windowstate --add MAXIMIZED_HORZ "$WIN"
wait_until 5 "the window to be maximized" maximized
sleep 0.5
key ctrl+comma
click_in 881 361
shot short-dialog
click_in 711 148
wait_until 5 "the layout saved as classic" [ "$(setting layout)" = classic ]
shot note
old=$APP_PID
click_in 950 419
wait_until 20 "the app to exit" exited "$old"
assert_clis_gone
app_relaunched
wait_until 5 "the saved bounds to be cleared" bounds_cleared
wait_until 5 "the relaunched window to be maximized" maximized
assert_eq "$(visible)" "0 0 1280 800" "the maximized window after the restart"
shot restarted-maximized

if grep -q panicked "$AHV_OUT/app.log"; then
	fail "the app panicked"
fi
app_stop
