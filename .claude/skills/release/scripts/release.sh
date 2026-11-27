#!/usr/bin/env bash
# Releases Agents Hub: bumps the version, runs the checks CI runs, then
# commits, tags and pushes, and waits for the release workflow to publish.
# Nothing is committed or pushed unless every check passes.
#
# Usage: release.sh X.Y.Z, on a clean main that is not behind origin/main.
set -euo pipefail

die() {
	echo "release: $*" >&2
	exit 1
}

version=${1:-}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
	echo "usage: release.sh X.Y.Z" >&2
	exit 2
}
tag="v$version"
cd "$(git rev-parse --show-toplevel)"

[ "$(git branch --show-current)" = main ] || die "not on main"
[ -z "$(git status --porcelain)" ] || die "uncommitted changes"
git fetch -q origin
git merge-base --is-ancestor origin/main main || die "main is behind origin/main"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
	[ -n "$(git ls-remote --tags origin "refs/tags/$tag")" ]; then
	die "tag $tag already exists"
fi
current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)
if [ "$current" = "$version" ] ||
	[ "$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n1)" != "$version" ]; then
	die "$version is not greater than $current"
fi

manifests=(Cargo.toml crates/agentshub-{app,core,platform,ui}/Cargo.toml)
sed -i "s/\"$current\"/\"$version\"/g" "${manifests[@]}"
cargo update -w --quiet
changed=$(git diff --name-only | sort | tr '\n' ' ')
expected=$(printf '%s\n' Cargo.lock "${manifests[@]}" | sort | tr '\n' ' ')
[ "$changed" = "$expected" ] ||
	die "the version bump changed $changed instead of $expected; nothing was committed or pushed"

# step NAME COMMAND...: runs a check, stopping the release if it fails.
step() {
	local name=$1
	shift
	echo "== $name"
	"$@" || die "stopped at $name; nothing was committed or pushed"
}
step fmt cargo fmt --check
step clippy cargo clippy --workspace --all-targets --locked -- -D warnings
step test cargo test --workspace --locked
step check-arch scripts/check-arch.sh
step deny cargo deny check advisories bans sources
step build cargo build
smoke() {
	local result
	result=$(.claude/skills/verify/scripts/run.sh .claude/skills/verify/scenarios/smoke.sh)
	echo "$result"
	[ "$(printf '%s\n' "$result" | head -n1)" = PASS ]
}
step smoke smoke

git commit -q -m "chore: $version release" -- Cargo.lock "${manifests[@]}"
git tag "$tag"
git push --atomic -q origin main "$tag" ||
	die "the push was refused, so nothing was released; the release commit and $tag are only local"

wait_for=${RELEASE_WORKFLOW_WAIT:-60}
waited=0
run=""
while :; do
	run=$(gh run list --workflow release.yml --branch "$tag" --json databaseId -L 1 | jq -r '.[0].databaseId // empty')
	[ -n "$run" ] && break
	[ "$waited" -lt "$wait_for" ] || die "no release workflow run for $tag after ${wait_for}s"
	sleep 5
	waited=$((waited + 5))
done
if ! gh run watch "$run" --exit-status --interval 30 >&2; then
	die "the release workflow failed: $(gh run view "$run" --json url | jq -r .url)"
fi

release=$(gh release view "$tag" --json url,assets)
for asset in agentshub-x86_64-linux.tar.gz agentshub-x86_64-linux.tar.gz.sha256; do
	jq -e --arg name "$asset" 'any(.assets[]; .name == $name)' <<<"$release" >/dev/null ||
		die "$tag was published without $asset"
done
echo "released $tag: $(jq -r .url <<<"$release")"
