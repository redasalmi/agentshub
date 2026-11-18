//! Removing a chat's worktree, keeping any commits only it had.

use std::path::Path;

use anyhow::bail;

use super::*;

/// What removing a chat's worktree did.
#[derive(Debug, PartialEq)]
pub enum Removal {
    Removed,
    /// It was removed, and the commits made on a detached HEAD in it, which
    /// no branch had, are on this new branch.
    Rescued(String),
    /// Git kept it, as when it has changes nobody committed. Its message says
    /// why.
    Kept(String),
}

/// Removes the worktree a chat ran in unless it has changes nobody
/// committed. Its branch stays, with what was committed on it. A worktree
/// already deleted by hand is forgotten from `project`'s repository.
pub fn remove(worktree: &Worktree, project: &Path) -> anyhow::Result<Removal> {
    let root = &worktree.root;
    if !root.exists() {
        // Git keeps its record of a worktree whose folder is gone until it
        // is pruned.
        git(project).args(["worktree", "prune"]).output().ok();
        return Ok(Removal::Removed);
    }
    // A folder that isn't a working tree of its own anymore would have git
    // look at whatever repository it is in, as a home folder kept in git.
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
    match repo(root) {
        Ok(Some(repo)) if repo.root == canonical => {}
        Ok(_) => {
            return Ok(Removal::Kept(format!(
                "{} isn't a git worktree anymore",
                root.display()
            )));
        }
        Err(err) => return Ok(Removal::Kept(format!("{err:#}"))),
    }
    // Removing a worktree deletes its HEAD's reflog, which is all that keeps
    // commits made on a detached HEAD, so they go on a branch first.
    let rescued = match unreachable_head(root) {
        Some(head) => Some(rescue(root, &head)?),
        None => None,
    };
    // Git refuses to remove anything but a linked worktree.
    let output = git(root)
        .args(["worktree", "remove"])
        .arg(root)
        .output()
        .context("Couldn't run git")?;
    if output.status.success() {
        return Ok(rescued.map_or(Removal::Removed, Removal::Rescued));
    }
    // The worktree stays, and its HEAD with it, so the branch isn't needed.
    if let Some(branch) = rescued {
        git(root)
            .args(["branch", "-q", "-D", &branch])
            .output()
            .ok();
    }
    Ok(Removal::Kept(
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

/// The commit a detached HEAD in `root` is at, unless a branch, a tag or a
/// remote's branch has it.
fn unreachable_head(root: &Path) -> Option<String> {
    let on_branch = git(root)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .is_ok_and(|output| output.status.success());
    if on_branch {
        return None;
    }
    let output = git(root)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let head = String::from_utf8(output.stdout).ok()?.trim().to_string();
    // Refs a worktree keeps to itself, as for a bisect, go with it, so only
    // the shared ones count.
    let output = git(root)
        .args([
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "--contains",
        ])
        .arg(&head)
        .args(["refs/heads", "refs/tags", "refs/remotes"])
        .output()
        .ok()?;
    // Without an answer, the commit is rescued: a branch too many is
    // better than lost work.
    let reachable = output.status.success() && !output.stdout.is_empty();
    (!reachable).then_some(head)
}

/// Puts a branch on `head`, named after the worktree, and returns its name.
fn rescue(root: &Path, head: &str) -> anyhow::Result<String> {
    let name = project_dir_name(root);
    let short = head.get(..8).unwrap_or(head);
    let branch = [
        format!("{BRANCH_PREFIX}{name}-detached"),
        format!("{BRANCH_PREFIX}{name}-detached-{short}"),
    ]
    .into_iter()
    .find(|branch| !branch_exists(root, branch))
    .unwrap_or_else(|| {
        format!(
            "{BRANCH_PREFIX}{name}-detached-{:08x}",
            uuid::Uuid::new_v4().as_fields().0
        )
    });
    let output = git(root)
        .args(["branch", &branch, head])
        .output()
        .context("Couldn't run git")?;
    if !output.status.success() {
        bail!(
            "git couldn't keep the commits made on a detached HEAD on a branch, so the \
             worktree stays: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(branch)
}
