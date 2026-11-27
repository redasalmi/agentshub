---
name: release
description: Release Agents Hub - merge the branch into main, pick the version from the commits, write release notes, run the release script and publish the notes. Use when asked to release, cut a release, ship a version or "merge into main and make a release".
---

# Release Agents Hub

A release goes from a branch or `main` to a published GitHub release.
`scripts/release.sh` does the fixed part (checks, commit, tag, push, the wait
for the workflow) the same way every time. Your part is the judgement: the
version and the release notes.

## 1. Check where you start

- The working tree must be clean. If it isn't, stop and say what is
  uncommitted.
- `git fetch -q origin`.
- On a feature branch, fast-forward it into `main`:
  `git switch main && git merge --ff-only <branch>`. If it can't be
  fast-forwarded, stop and ask; never merge or rebase on your own.
- `main` must not be behind `origin/main` (`git merge-base --is-ancestor
  origin/main main`). If it is, stop and say how they differ.

## 2. Pick the version

List the commits since the last tag:
`git log --format='%h %s' "$(git describe --tags --abbrev=0 --match 'v*')"..HEAD`.

- With no commits, say there is nothing to release, and stop.
- A `!` after the type or a `BREAKING CHANGE` footer: minor bump while the
  version is below 1.0.
- Otherwise, any `feat`: minor bump (0.19.1 → 0.20.0).
- Otherwise: patch bump (0.19.1 → 0.19.2).
- A version the developer gave (`/release 0.20.0`) wins.

## 3. Write the notes

What a user would notice, in plain words, one line each, under **New** and
**Fixed**. Leave a section out when it would be empty.

- Leave out `chore`, `docs`, `test`, `ci` and `refactor` commits, unless a
  user would see the change. `style` commits change what users see: include
  them.
- Merge commits that make one change into one line. Name what the user can
  do or what stopped going wrong, not the code.
- End with GitHub's compare link.

```markdown
## New
- Open a chat's folder in your code editor from the chat header, or with Ctrl+Shift+E.

## Fixed
- Editor icons look sharp.

**Full Changelog**: https://github.com/redasalmi/agentshub/compare/v0.19.0...v0.19.1
```

## 4. Ask once

Show the version, the commit list and the notes, and wait for a yes. That
yes is the go-ahead to push and publish; without it, stop here.

## 5. Run the script

```sh
.claude/skills/release/scripts/release.sh X.Y.Z
```

Give it a 30-minute timeout: the checks and the release workflow take most
of that. Its last line says how it went:

- `released vX.Y.Z: <url>`: go on to step 6.
- `release: stopped at <check>; nothing was committed or pushed`: report the
  failing check with its output. The version bump stays in the working tree.
- `release: the push was refused…`: someone pushed to `main` meanwhile. Nothing
  was released; the release commit and tag are only local. Report it and ask.
- `release: the release workflow failed: <url>`: the tag is pushed. Report
  the run's link and leave the tag alone.
- Any other `release:` or `usage:` line: report it as it is.

## 6. Publish the notes

Write the notes to a file in the scratchpad, then
`gh release edit vX.Y.Z --notes-file <file>`. Report the release link, the
version, and that the checks and the workflow passed.
