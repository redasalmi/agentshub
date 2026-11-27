#!/usr/bin/env bash
# Runs release.sh against a throwaway clone of this repo whose origin is a
# local bare repo, with cargo and gh stubbed, so nothing real is pushed.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
real_cargo=$(command -v cargo)
release="$here/release.sh"
passed=0
failed=0

export GIT_CONFIG_COUNT=2
export GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false
export GIT_CONFIG_KEY_1=tag.gpgsign GIT_CONFIG_VALUE_1=false

check() {
	if [ "$2" = 0 ]; then
		echo "ok $1"
		passed=$((passed + 1))
	else
		echo "FAIL $1"
		failed=$((failed + 1))
	fi
}

# fixture: a clone at $work on main, pushed to $remote, with stubs first on
# PATH logging to $CALLS.
fixture() {
	root=$(mktemp -d)
	remote="$root/remote.git"
	work="$root/work"
	export CALLS="$root/calls.log" GH_STATE="$root/gh-runs"
	git init -q --bare --initial-branch=main "$remote"
	git clone -q "$repo" "$work"
	git -C "$work" checkout -q -B main
	git -C "$work" remote set-url origin "$remote"
	mkdir -p "$root/bin"
	cat >"$root/bin/cargo" <<EOF
#!/usr/bin/env bash
if [ "\$1" = update ]; then exec "$real_cargo" "\$@"; fi
echo "\$1" >>"\$CALLS"
if [ "\$1" = build ] && [ -n "\${RACE_CLONE:-}" ]; then
	git -C "\$RACE_CLONE" commit -q --allow-empty -m race
	git -C "\$RACE_CLONE" push -q origin HEAD:main
fi
[ "\$1" != "\${CARGO_FAIL:-}" ]
EOF
	cat >"$root/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"run list")
	n=$(cat "$GH_STATE" 2>/dev/null || echo 0)
	echo $((n + 1)) >"$GH_STATE"
	if [ "$n" -lt "${GH_EMPTY_RUNS:-0}" ]; then echo '[]'; else echo '[{"databaseId":1}]'; fi
	;;
"run watch") exit 0 ;;
"run view") echo '{"url":"https://example.invalid/runs/1"}' ;;
"release view")
	echo '{"url":"https://example.invalid/releases/'"$3"'","assets":[{"name":"agentshub-x86_64-linux.tar.gz"},{"name":"agentshub-x86_64-linux.tar.gz.sha256"}]}'
	;;
*) exit 1 ;;
esac
EOF
	printf '#!/bin/sh\necho check-arch >>"$CALLS"\n' >"$work/scripts/check-arch.sh"
	printf '#!/bin/sh\necho smoke >>"$CALLS"\necho PASS\n' >"$work/.claude/skills/verify/scripts/run.sh"
	chmod +x "$root/bin/cargo" "$root/bin/gh" "$work/scripts/check-arch.sh" \
		"$work/.claude/skills/verify/scripts/run.sh"
	git -C "$work" commit -qam "test stubs"
	git -C "$work" push -q origin main
	PATH="$root/bin:$PATH"
	export PATH
	: >"$CALLS"
	current=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$work/Cargo.toml" | head -n1)
}

# run ARGS...: runs release.sh in $work; sets $status and $last.
run() {
	out=$(cd "$work" && "$release" "$@" 2>&1)
	status=$?
	last=$(printf '%s\n' "$out" | tail -n1)
}

head_of() { git -C "$1" rev-parse "${2:-HEAD}"; }

happy_path() {
	fixture
	run 9.9.9
	local ok=0
	[ "$status" = 0 ] || ok=1
	[ "$(git -C "$work" log -1 --format=%s)" = "chore: 9.9.9 release" ] || ok=1
	git -C "$remote" rev-parse -q --verify refs/tags/v9.9.9 >/dev/null || ok=1
	[ "$(head_of "$remote" main)" = "$(head_of "$work" main)" ] || ok=1
	[ "$(git -C "$work" show --name-only --format= HEAD | sort | tr '\n' ' ')" = \
		"Cargo.lock Cargo.toml crates/agentshub-app/Cargo.toml crates/agentshub-core/Cargo.toml crates/agentshub-platform/Cargo.toml crates/agentshub-ui/Cargo.toml " ] || ok=1
	[ "$(tr '\n' ' ' <"$CALLS")" = "fmt clippy test check-arch deny build smoke " ] || ok=1
	[ "$last" = "released v9.9.9: https://example.invalid/releases/v9.9.9" ] || ok=1
	[ "$ok" = 0 ] || printf '%s\n' "$out" | tail -n5
	check "happy path" "$ok"
}

# refuses NAME EXPECTED-LAST-LINE ARGS...: release.sh must exit non-zero
# with that last line, leaving HEAD and the tree as they were.
refuses() {
	local name=$1 expected=$2
	shift 2
	local before
	before=$(head_of "$work")
	run "$@"
	local ok=0
	[ "$status" != 0 ] || ok=1
	[ "$last" = "$expected" ] || ok=1
	[ "$(head_of "$work")" = "$before" ] || ok=1
	git -C "$work" diff --quiet -- Cargo.toml || ok=1
	[ "$ok" = 0 ] || printf 'got: %s (exit %s)\n' "$last" "$status"
	check "$name" "$ok"
}

guard_dirty_tree() {
	fixture
	touch "$work/stray"
	refuses "guard: dirty tree" "release: uncommitted changes" 9.9.9
}

guard_not_on_main() {
	fixture
	git -C "$work" switch -q -c x
	refuses "guard: not on main" "release: not on main" 9.9.9
}

guard_behind_origin() {
	fixture
	git clone -q "$remote" "$root/other"
	git -C "$root/other" commit -q --allow-empty -m other
	git -C "$root/other" push -q origin HEAD:main
	refuses "guard: behind origin" "release: main is behind origin/main" 9.9.9
}

guard_remote_tag() {
	fixture
	git clone -q "$remote" "$root/other"
	git -C "$root/other" tag v9.9.9
	git -C "$root/other" push -q origin v9.9.9
	refuses "guard: remote tag" "release: tag v9.9.9 already exists" 9.9.9
}

guard_not_greater() {
	fixture
	refuses "guard: not greater" "release: 0.0.1 is not greater than $current" 0.0.1
}

guard_bad_version() {
	fixture
	refuses "guard: bad version" "usage: release.sh X.Y.Z" v1.2.3
}

failing_check() {
	fixture
	local before
	before=$(head_of "$work")
	CARGO_FAIL=clippy run 9.9.9
	local ok=0
	[ "$status" != 0 ] || ok=1
	[ "$last" = "release: stopped at clippy; nothing was committed or pushed" ] || ok=1
	[ "$(head_of "$work")" = "$before" ] || ok=1
	! git -C "$work" rev-parse -q --verify refs/tags/v9.9.9 >/dev/null || ok=1
	! git -C "$remote" rev-parse -q --verify refs/tags/v9.9.9 >/dev/null || ok=1
	[ "$(git -C "$work" status --short | wc -l)" = 6 ] || ok=1
	[ "$ok" = 0 ] || printf 'got: %s (exit %s)\n' "$last" "$status"
	check "failing check stops before commit" "$ok"
}

main_ahead_of_origin() {
	fixture
	git -C "$work" commit -q --allow-empty -m "feat: merged feature"
	run 9.9.9
	local ok=0
	[ "$status" = 0 ] || ok=1
	[ "$(head_of "$remote" main)" = "$(head_of "$work" main)" ] || ok=1
	git -C "$remote" log --format=%s main | grep -x "feat: merged feature" >/dev/null || ok=1
	[ "$ok" = 0 ] || printf 'got: %s (exit %s)\n' "$last" "$status"
	check "main ahead of origin is released" "$ok"
}

refused_push() {
	fixture
	git clone -q "$remote" "$root/other"
	RACE_CLONE="$root/other" run 9.9.9
	local ok=0
	[ "$status" != 0 ] || ok=1
	case $last in "release: the push was refused"*) ;; *) ok=1 ;; esac
	! git -C "$remote" rev-parse -q --verify refs/tags/v9.9.9 >/dev/null || ok=1
	[ "$ok" = 0 ] || printf 'got: %s (exit %s)\n' "$last" "$status"
	check "a refused push publishes no tag" "$ok"
}

waits_for_run() {
	fixture
	GH_EMPTY_RUNS=2 RELEASE_WORKFLOW_WAIT=10 run 9.9.9
	local ok=0
	[ "$status" = 0 ] || ok=1
	[ "$(cat "$GH_STATE")" = 3 ] || ok=1
	[ "$ok" = 0 ] || printf 'got: %s (exit %s)\n' "$last" "$status"
	check "waits for the tag's run" "$ok"
}

for case in happy_path guard_dirty_tree guard_not_on_main guard_behind_origin \
	guard_remote_tag guard_not_greater guard_bad_version failing_check main_ahead_of_origin \
	refused_push waits_for_run; do
	(
		passed=0 failed=0
		"$case"
		rc=$failed
		rm -rf "$root"
		exit "$rc"
	)
	if [ $? = 0 ]; then passed=$((passed + 1)); else failed=$((failed + 1)); fi
done
echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
