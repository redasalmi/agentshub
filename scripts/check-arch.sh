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

# core must build and test without the UI framework.
scan crates/agentshub-core/src 'gpui_kit|gpui::|\bWindow\b|\bEntity<|\bContext<'
# ui-classic renders what the runtime holds; it does not persist, run
# processes or touch the filesystem.
scan crates/agentshub-ui-classic/src '\bStore\b|rusqlite|std::fs|std::process|portable_pty|notify::|agentshub_core::(lifecycle|reports|sessions|worktree|process|transcript)'
# ui-kit is what the layouts share; it knows no layout.
scan crates/agentshub-ui-kit/src '\bStore\b|rusqlite|std::fs|std::process|portable_pty|notify::|agentshub_core::(lifecycle|reports|sessions|worktree|process|transcript)|agentshub_ui_classic|agentshub_ui_triage|agentshub_ui::'
# runtime sits below the UI.
scan crates/agentshub-runtime/src 'agentshub_ui|crate::ui'
# platform is an adapter layer.
scan crates/agentshub-platform/src 'crate::ui|crate::app'

# The single-crate layout (before the workspace split) is checked too.
scan src/core 'gpui_kit|gpui::|\bWindow\b|\bEntity<|\bContext<'
scan src/ui 'crate::store|rusqlite|std::fs|std::process|Store'

exit "$status"
