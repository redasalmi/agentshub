# Starts a chat in a worktree of a project with changes nobody committed and
# ignored files, and checks the CLI runs in a copy of the project as it is.
# Covers: worktree creation, copying the working tree, the CLI's cwd.

project="$AHV_WORK/project"
seed_project "$project"

git -C "$project" -c user.name=verify -c user.email=verify@example.com \
	commit -q --allow-empty -m init
cd "$project"
printf '/.env\nnode_modules/\n' >.gitignore
printf 'tracked\n' >tracked.txt
printf 'gone\n' >gone.txt
printf '#!/bin/sh\necho ran\n' >run.sh
chmod 755 run.sh
git add .
git -c user.name=verify -c user.email=verify@example.com commit -q -m files
printf 'changed\n' >>tracked.txt
rm gone.txt
printf 'untracked\n' >notes.md
printf 'TOKEN=verify\n' >.env
mkdir -p node_modules/pkg node_modules/.bin empty
printf 'module.exports = 1\n' >node_modules/pkg/index.js
ln -s ../pkg/index.js node_modules/.bin/pkg
cd - >/dev/null

sqlite3 "$DB" "INSERT OR REPLACE INTO settings (key, value) VALUES ('worktree_by_default', 1);"

app_start
key ctrl+1
key ctrl+shift+n
shot new-chat
key Return

wait_until 30 "fake claude to start" cli_started claude
started=$(cli_started claude)
cwd=$(field cwd "$started")
log "claude started: $started"
case $cwd in
"$XDG_DATA_HOME/agentshub/worktrees/project/"*) ;;
*) fail "claude's working directory isn't a worktree: $cwd" ;;
esac
assert_eq "$(git -C "$cwd" rev-parse HEAD)" "$(git -C "$project" rev-parse HEAD)" "the worktree's commit"
case $(git -C "$cwd" branch --show-current) in
agentshub/*) ;;
*) fail "the worktree isn't on an agentshub branch" ;;
esac

# Every path, with its kind, mode, size, modification time and link target.
listing() {
	(cd "$1" && find . -path ./.git -prune -o \
		\( -type f -printf '%P f %m %s %T@\n' \) -o \
		\( -type l -printf '%P l %l\n' \) -o \
		\( -type d -printf '%P d %m\n' \) | LC_ALL=C sort)
}
listing "$project" >"$AHV_OUT/project.list"
listing "$cwd" >"$AHV_OUT/worktree.list"
diff -u "$AHV_OUT/project.list" "$AHV_OUT/worktree.list" >&2 ||
	fail "the worktree's files differ from the project's"
diff -r --no-dereference -x .git "$project" "$cwd" >&2 ||
	fail "the worktree's contents differ from the project's"
assert_eq "$(git -C "$cwd" status --porcelain | LC_ALL=C sort)" \
	"$(git -C "$project" status --porcelain | LC_ALL=C sort)" "the worktree's git status"
log "worktree matches the project: $(wc -l <"$AHV_OUT/project.list") paths"

key ctrl+grave
type_text "pwd; cat .env; ls -A; git status --short; ./run.sh; touch $AHV_WORK/shell-ran"
key Return
wait_until 10 "the commands in the project shell" test -f "$AHV_WORK/shell-ran"
shot worktree-shell

app_stop
assert_clis_gone
