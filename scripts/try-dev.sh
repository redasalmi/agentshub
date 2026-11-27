#!/usr/bin/env bash
# Runs a dev build of Agents Hub on your desktop with your real claude, codex
# and pi, but with a database and session bus of its own, so the installed
# copy, its chats and its running agents are left alone.
#
# Usage: scripts/try-dev.sh [--keep] [PROJECT_DIR...]
#
# The new database starts with the given projects, or this repository. The
# agents still keep their sessions in their own folders (~/.claude, ~/.codex,
# ~/.pi), as they do when the installed copy runs them. Close the window or
# press Ctrl+C to quit; the database is deleted then, unless --keep is given.
#
# AGENTSHUB_BIN runs another binary instead of building target/debug/agentshub.
set -euo pipefail

keep=""
projects=()
for arg in "$@"; do
	case $arg in
	--keep) keep=1 ;;
	-h | --help)
		sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
		exit 0
		;;
	*)
		[ -d "$arg" ] || { echo "not a folder: $arg" >&2; exit 2; }
		projects+=("$(realpath "$arg")")
		;;
	esac
done

for tool in dbus-daemon sqlite3; do
	command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
[ ${#projects[@]} -gt 0 ] || projects=("$repo")
if [ -n "${AGENTSHUB_BIN:-}" ]; then
	bin=$(realpath "$AGENTSHUB_BIN")
else
	cargo build --manifest-path "$repo/Cargo.toml"
	bin="$repo/target/debug/agentshub"
fi
[ -x "$bin" ] || { echo "no binary at $bin" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/agentshub-dev.XXXXXX")
mkdir -p "$work/data" "$work/bin"
db="$work/data/agentshub/agentshub.db"
app=""
bus=""

# Stops what this run started by its PIDs, never by name, which would match
# the installed copy too. The app stops its agents as it quits.
stop_app() {
	[ -n "$app" ] || return 0
	kill -TERM "$app" 2>/dev/null || true
	wait "$app" 2>/dev/null || true
	app=""
}
cleanup() {
	stop_app
	[ -n "$bus" ] && kill -TERM "$bus" 2>/dev/null || true
	if [ -n "$keep" ]; then echo "kept $work" >&2; else rm -rf "$work"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# A session bus of its own keeps the dev build from handing off to the
# installed copy, and from stopping that copy's agents as leftovers. It also
# has no tray, so closing the window quits.
{ read -r bus_address; read -r bus; } < <(dbus-daemon --session --fork --print-address=1 --print-pid=1)

# Fonts installed for you alone live in the data folder too.
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
[ -d "$data_home/fonts" ] && ln -s "$data_home/fonts" "$work/data/fonts"

# So are the editors installed for you alone and their icons, which the chat
# header's editor button lists.
for dir in applications icons; do
	if [ -d "$data_home/$dir" ]; then ln -s "$data_home/$dir" "$work/data/$dir"; fi
done

# The agents get your own data folder and session bus back, so they work as
# they do anywhere else.
restore=""
if [ -n "${XDG_DATA_HOME:-}" ]; then
	restore+="export XDG_DATA_HOME=$(printf %q "$XDG_DATA_HOME")"$'\n'
else
	restore+="unset XDG_DATA_HOME"$'\n'
fi
if [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
	restore+="export DBUS_SESSION_BUS_ADDRESS=$(printf %q "$DBUS_SESSION_BUS_ADDRESS")"$'\n'
fi
for cli in claude codex pi; do
	real=$(command -v "$cli" || true)
	[ -n "$real" ] || continue
	printf '#!/usr/bin/env bash\n%sexec %q "$@"\n' "$restore" "$real" >"$work/bin/$cli"
	chmod +x "$work/bin/$cli"
done

start_app() {
	XDG_DATA_HOME="$work/data" PATH="$work/bin:$PATH" DBUS_SESSION_BUS_ADDRESS="$bus_address" \
		"$bin" "$@" &
	app=$!
}

# The app creates its database on its first start.
start_app >>"$work/app.log" 2>&1
for _ in $(seq 150); do
	[ -f "$db" ] && break
	kill -0 "$app" 2>/dev/null || { echo "the app exited, see $work/app.log" >&2; keep=1; exit 1; }
	sleep 0.2
done
[ -f "$db" ] || { echo "no database after 30s, see $work/app.log" >&2; keep=1; exit 1; }
stop_app

quote() { printf '%s' "$1" | sed "s/'/''/g"; }
position=0
for project in "${projects[@]}"; do
	position=$((position - 1))
	sqlite3 "$db" "INSERT INTO projects (path, name, created_at, position)
		VALUES ('$(quote "$project")', '$(quote "$(basename "$project")")',
		        strftime('%s','now'), $position);"
	echo "project: $project"
done
echo "database: $db"

start_app
wait "$app" || true
app=""
