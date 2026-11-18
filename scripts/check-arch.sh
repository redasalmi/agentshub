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
	if grep -rnE "$*" "$dir" --include='*.rs' | grep -v '^[^:]*:[0-9]*: *//' >/dev/null; then
		grep -rnE "$*" "$dir" --include='*.rs' | grep -v '^[^:]*:[0-9]*: *//' >&2
		fail "$dir uses a layer it must not import: $*"
	fi
}

# core must build and test without the UI framework.
scan crates/agentshub-core/src 'gpui_kit|gpui::|Window|Entity<|Context<'
# ui renders; it does not persist or run processes.
scan crates/agentshub-ui/src 'crate::store|rusqlite|std::fs|std::process|Store'
# platform is an adapter layer.
scan crates/agentshub-platform/src 'crate::ui|crate::app'

# The single-crate layout (before the workspace split) is checked too.
scan src/core 'gpui_kit|gpui::|Window|Entity<|Context<'
scan src/ui 'crate::store|rusqlite|std::fs|std::process|Store'

exit "$status"
