#!/usr/bin/env bash
# Runs one scenario against a dev build inside a hidden KWin of its own.
# Usage: run.sh SCENARIO [OUT_DIR]
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
scenario=$(realpath "${1:?usage: run.sh SCENARIO [OUT_DIR]}")
bin=$(realpath -m "${AGENTSHUB_BIN:-$repo/target/debug/agentshub}")
[ -x "$bin" ] || { echo "no binary at $bin: run cargo build first" >&2; exit 2; }
for tool in kwin_wayland Xwayland dbus-run-session xdotool spectacle sqlite3 jq; do
	command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done

name=$(basename "$scenario" .sh)
out=$(realpath -m "${2:-$repo/target/verify/$(date +%Y%m%d-%H%M%S)-$name}")
# What an earlier run left in a reused folder would pass for this one's, and
# nothing in a folder the caller gave is the run's to delete.
if [ -n "$(ls -A "$out" 2>/dev/null)" ]; then
	echo "$out isn't empty: give a new folder for the artifacts" >&2
	exit 2
fi
# Wayland socket paths must stay under 108 bytes, so the runtime dir is short.
work=$(mktemp -d /tmp/ahv.XXXXXX)
mkdir -p "$work"/{rt,cfg,data,home,cache,bin} "$out"
chmod 700 "$work/rt"
# KWin splits the session command at spaces, which the repo's path may have.
ln -s "$here/session.sh" "$work/session.sh"
printf '[Xwayland]\nXwaylandEisNoPrompt=true\n' >"$work/cfg/kwinrc"
for cli in claude codex pi; do ln -s "$here/fake-cli" "$work/bin/$cli"; done

status=0
env -i \
	HOME="$work/home" \
	PATH="$work/bin:/usr/bin:/bin" \
	LANG=C.UTF-8 \
	XDG_RUNTIME_DIR="$work/rt" \
	XDG_CONFIG_HOME="$work/cfg" \
	XDG_CACHE_HOME="$work/cache" \
	XDG_DATA_HOME="$work/data" \
	AHV_BIN="$bin" AHV_WORK="$work" AHV_OUT="$out" AHV_LIB="$here/lib.sh" AHV_SCENARIO="$scenario" \
	timeout --kill-after=10 "${AHV_TIMEOUT:-300}" \
	dbus-run-session -- kwin_wayland --virtual --no-lockscreen --width 1280 --height 800 \
	--socket hubtest --xwayland --exit-with-session "bash $work/session.sh" \
	>"$out/kwin.log" 2>&1 || status=$?

result=$(cat "$out/result" 2>/dev/null || echo "FAIL: no result (kwin exit $status, see $out/kwin.log)")

# Only what this run started: the apps it launched, by PID while they are
# still that binary, the fake CLIs, whose command lines name this run's own
# $work/bin, and the session's bus, KWin and Xwayland, whose environment has
# this run's AHV_WORK. Never match by name or binary path: the installed copy,
# or another run of the same build, would match too.
left=$(pgrep -f -- "$work/bin/" || true)
for environ in $(grep -lzxF "AHV_WORK=$work" /proc/[0-9]*/environ 2>/dev/null); do
	pid=${environ#/proc/}
	left="$left ${pid%/environ}"
done
while read -r pid; do
	case $(readlink "/proc/$pid/exe" 2>/dev/null) in
	"$bin" | "$bin (deleted)") left="$left $pid" ;;
	esac
done < <(cat "$work/app.pids" 2>/dev/null)
left=$(printf '%s\n' $left | sort -un | tr '\n' ' ')
left=${left% }
if [ -n "$left" ]; then
	echo "LEFTOVER processes:" >&2
	ps -o pid=,args= -p $left >&2 || true
	kill -KILL $left 2>/dev/null || true
	result="FAIL: leftover processes ($result)"
fi

if [ -n "${AHV_KEEP:-}" ]; then echo "kept $work" >&2; else rm -rf "$work"; fi
echo "$result"
echo "artifacts: $out"
case $result in PASS*) exit 0 ;; *) exit 1 ;; esac
