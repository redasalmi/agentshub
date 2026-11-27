#!/bin/sh
# Checks the layer boundaries described in ARCHITECTURE.md.
#
# Run from the repository root: scripts/check-arch.sh
set -eu

status=0

fail() {
	printf 'architecture: %s\n' "$1" >&2
	status=1
}

scan() {
	dir=$1
	shift
	[ -d "$dir" ] || return 0
	[ -n "$*" ] || return 0
	if grep -rnE "$*" "$dir" --include='*.rs' | grep -v '^[^:]*:[0-9]*: *//' | grep -v '/tests\.rs:' >/dev/null; then
		grep -rnE "$*" "$dir" --include='*.rs' | grep -v '^[^:]*:[0-9]*: *//' | grep -v '/tests\.rs:' >&2
		fail "$dir uses a layer it must not import: $*"
	fi
}

scan_filtered() {
	dir=$1
	pattern=$2
	skip=$3
	if grep -rnE "$pattern" "$dir" --include='*.rs' | grep -v '^[^:]*:[0-9]*: *//' | grep -v '/tests\.rs:' | grep -v "$skip" >/dev/null; then
		grep -rnE "$pattern" "$dir" --include='*.rs' | grep -v '^[^:]*:[0-9]*: *//' | grep -v '/tests\.rs:' | grep -v "$skip" >&2
		fail "$dir uses a layer it must not import: $pattern"
	fi
}

# core must build and test without the UI framework.
scan crates/agentshub-core/src 'gpui_kit|gpui::|\bWindow\b|\bEntity<|\bContext<'
# ui renders; it does not persist or run processes.
scan_filtered crates/agentshub-ui/src 'crate::store|rusqlite|std::fs|std::process|portable_pty|Store' '/workspace/'
# runtime sits below the UI.
scan crates/agentshub-runtime/src 'agentshub_ui|crate::ui'
# platform is an adapter layer.
scan crates/agentshub-platform/src 'crate::ui|crate::app'

# The single-crate layout (before the workspace split) is checked too.
scan src/core 'gpui_kit|gpui::|\bWindow\b|\bEntity<|\bContext<'
scan src/ui 'crate::store|rusqlite|std::fs|std::process|Store'

exit "$status"
