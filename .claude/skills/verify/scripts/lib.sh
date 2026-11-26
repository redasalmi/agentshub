# Helpers for scenarios. Sourced inside the hidden session by session.sh.

DB="$XDG_DATA_HOME/agentshub/agentshub.db"
CLI_LOG="$AHV_OUT/fake-cli.log"
APP_PID=""
WIN=""
SHOTS=0
touch "$CLI_LOG"

log() { echo "[$(date +%T)] $*" | tee -a "$AHV_OUT/run.log" >&2; }

fail() {
	log "FAIL: $*"
	echo "FAIL: $*" >"$AHV_OUT/result"
	shot fail || true
	exit 1
}

# wait_until SECONDS DESCRIPTION CMD...: polls CMD every 0.2s.
wait_until() {
	local secs=$1 what=$2
	shift 2
	local end=$((SECONDS + secs))
	until "$@" >/dev/null 2>&1; do
		[ "$SECONDS" -lt "$end" ] || fail "timed out after ${secs}s waiting for: $what"
		sleep 0.2
	done
}

app_running() { [ -n "$APP_PID" ] && kill -0 "$APP_PID" 2>/dev/null; }

# The apps this run started that are still running, by the PIDs it recorded,
# while each is still the binary under test.
own_apps() {
	local pid
	while read -r pid; do
		case $(readlink "/proc/$pid/exe" 2>/dev/null) in
		"$AHV_BIN" | "$AHV_BIN (deleted)") echo "$pid" ;;
		esac
	done < <(cat "$AHV_WORK/app.pids" 2>/dev/null)
}

find_window() { WIN=$(xdotool search --class agentshub 2>/dev/null | tail -1) && [ -n "$WIN" ]; }

# The app runs on Xwayland so xdotool can drive it.
app_start() {
	env -u WAYLAND_DISPLAY "$AHV_BIN" >>"$AHV_OUT/app.log" 2>&1 &
	APP_PID=$!
	echo "$APP_PID" >>"$AHV_WORK/app.pids"
	log "app started pid=$APP_PID"
	wait_until 30 "the agentshub window" find_window
	xdotool windowactivate --sync "$WIN" 2>/dev/null || true
	sleep 1.5
}

# Stops the test copy by its PID, never by name.
app_stop() {
	app_running || return 0
	kill -TERM "$APP_PID"
	local end=$((SECONDS + 10))
	while kill -0 "$APP_PID" 2>/dev/null; do
		if [ "$SECONDS" -ge "$end" ]; then
			kill -KILL "$APP_PID"
			fail "app pid=$APP_PID ignored SIGTERM for 10s"
		fi
		sleep 0.2
	done
	log "app stopped pid=$APP_PID"
	APP_PID=""
	WIN=""
}

key() {
	xdotool key --clearmodifiers "$@"
	sleep 0.4
}

type_text() {
	xdotool type --delay 40 -- "$1"
	sleep 0.3
}

click() {
	xdotool mousemove "$1" "$2"
	sleep 0.3
	xdotool click 1
	sleep 0.4
}

# shot NAME: a full-screen shot of the virtual screen.
shot() {
	SHOTS=$((SHOTS + 1))
	local file
	file="$AHV_OUT/$(printf %02d "$SHOTS")-$1.png"
	sleep 0.5
	spectacle -b -n -f -o "$file" >/dev/null 2>&1 || log "screenshot failed: $file"
	log "shot $file"
}

sql() { sqlite3 -readonly "$DB" "$1"; }

# Creates the database by starting the app once, if it isn't there yet.
ensure_db() {
	[ -f "$DB" ] && return 0
	app_start
	wait_until 10 "the database" test -f "$DB"
	app_stop
}

# seed_project DIR: adds a git repo at DIR as a project while the app is
# stopped, since there is no file picker in the hidden session.
seed_project() {
	app_running && fail "seed_project needs the app stopped"
	ensure_db
	mkdir -p "$1"
	git -C "$1" init -q
	sqlite3 "$DB" "INSERT INTO projects (path, name, created_at, position)
		VALUES ('$1', '$(basename "$1")', strftime('%s','now'),
		        (SELECT COALESCE(MIN(position), 0) - 1 FROM projects));"
	log "seeded project $1"
}

# The fake CLI logs "start AGENT pid=N session=ID resume=ID cwd=DIR
# config=DIR configvar=set|unset args=...", "missing AGENT session=ID
# config=DIR" when it can't resume, "input AGENT LINE" and "exit AGENT pid=N".
cli_started() { grep -E "^start $1 " "$CLI_LOG" | tail -1; }
cli_count() { grep -cE "^start $1 " "$CLI_LOG" || true; }
field() { sed -nE "s/.* $1=([^ ]*).*/\1/p" <<<"$2"; }
cli_got_input() { grep -qF "input $1 $2" "$CLI_LOG"; }

# Every fake CLI that started must have exited.
assert_clis_gone() {
	local pid
	for pid in $(sed -nE 's/^start [a-z]+ pid=([0-9]+).*/\1/p' "$CLI_LOG"); do
		wait_until 5 "fake CLI pid=$pid to exit" bash -c "! kill -0 $pid"
	done
}

assert_eq() { [ "$1" = "$2" ] || fail "$3: expected '$2', got '$1'"; }

# seed_editor DESKTOP_ID [SVG]: installs a fake editor as the desktop entry
# DESKTOP_ID, which logs "DESKTOP_ID|cwd|args" to $EDITOR_LOG and exits. With
# SVG, the entry names an icon of that SVG.
EDITOR_LOG="$AHV_OUT/editor.log"
seed_editor() {
	local id=$1 bin="$AHV_WORK/editors/$1"
	mkdir -p "$AHV_WORK/editors" "$XDG_DATA_HOME/applications"
	printf '#!/bin/sh\necho "%s|$(pwd -P)|$*" >>"%s"\n' "$id" "$EDITOR_LOG" >"$bin"
	chmod +x "$bin"
	{
		printf '[Desktop Entry]\nType=Application\nName=%s\nExec=%s %%U\n' "$id" "$bin"
		if [ -n "${2:-}" ]; then
			mkdir -p "$XDG_DATA_HOME/icons/hicolor/scalable/apps"
			printf '%s' "$2" >"$XDG_DATA_HOME/icons/hicolor/scalable/apps/agentshub-test-$id.svg"
			printf 'Icon=agentshub-test-%s\n' "$id"
		fi
	} >"$XDG_DATA_HOME/applications/$id.desktop"
	log "seeded editor $id"
}
