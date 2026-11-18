#!/usr/bin/env bash
# Runs inside the hidden KWin, which has set DISPLAY.
set -uo pipefail
source "$AHV_LIB"
log "DISPLAY=$DISPLAY scenario=$AHV_SCENARIO"
( set -e; source "$AHV_SCENARIO" )
status=$?
for pid in $(own_apps); do kill -TERM "$pid" 2>/dev/null || true; done
if [ "$status" -eq 0 ]; then echo PASS >"$AHV_OUT/result"; else
	[ -s "$AHV_OUT/result" ] || echo "FAIL: scenario exited $status" >"$AHV_OUT/result"
fi
log "$(cat "$AHV_OUT/result")"
