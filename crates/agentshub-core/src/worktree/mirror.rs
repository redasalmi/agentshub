//! Copying a project's working tree into its new worktree as it is, with the
//! changes nobody committed and the files git ignores, like `.env` and
//! `node_modules`, so the agent can build and test there right away.

use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context as _, bail};

/// Makes `dst`, a new checkout of `src`'s repository, hold what `src`
/// holds, down to file modes and modification times, unless `stopped` says
/// to stop. Each one's `.git` stays its own, and a `.git` file in a
/// subfolder isn't copied: it ties a submodule or another worktree to its
/// repository, which git in the copy would work on instead. `skip`, where
/// the worktrees are kept, isn't copied, for a repository that holds it.
pub(super) fn mirror(
    src: &Path,
    dst: &Path,
    skip: &Path,
    stopped: &dyn Fn() -> bool,
) -> anyhow::Result<()> {
    mirror_dir(src, dst, true, skip, stopped)
}

fn mirror_dir(
    src: &Path,
    dst: &Path,
    top: bool,
    skip: &Path,
    stopped: &dyn Fn() -> bool,
) -> anyhow::Result<()> {
    let names: HashSet<_> = fs::read_dir(src)
        .and_then(|entries| entries.map(|entry| Ok(entry?.file_name())).collect())
        .with_context(|| format!("Couldn't read {}", src.display()))?;
    // What the checkout has that the project doesn't, as a file deleted
    // without a commit.
    for entry in fs::read_dir(dst).with_context(|| format!("Couldn't read {}", dst.display()))? {
        let name = entry?.file_name();
        if name != ".git" && !names.contains(&name) {
            remove(&dst.join(name))?;
        }
    }
    for name in names {
        if stopped() {
            bail!("The worktree was abandoned.");
        }
        let (from, to) = (src.join(&name), dst.join(&name));
        let meta = fs::symlink_metadata(&from)
            .with_context(|| format!("Couldn't read {}", from.display()))?;
        if from == skip || (name == ".git" && (top || !meta.is_dir())) {
            continue;
        }
        copy(&from, &to, &meta, skip, stopped)
            .with_context(|| format!("Couldn't copy {}", from.display()))?;
    }
    Ok(())
}

fn copy(
    from: &Path,
    to: &Path,
    meta: &Metadata,
    skip: &Path,
    stopped: &dyn Fn() -> bool,
) -> anyhow::Result<()> {
    let kind = meta.file_type();
    if kind.is_dir() {
        if !fs::symlink_metadata(to).is_ok_and(|meta| meta.is_dir()) {
            remove(to)?;
            fs::create_dir(to)?;
        }
        mirror_dir(from, to, false, skip, stopped)?;
        // Last, as a folder that can't be written to is.
        fs::set_permissions(to, meta.permissions())?;
    } else if kind.is_symlink() {
        remove(to)?;
        symlink(&fs::read_link(from)?, to)?;
    } else if kind.is_file() {
        remove(to)?;
        // A clone where the filesystem can, as btrfs and XFS do, so a large
        // `node_modules` takes no room of its own until it changes.
        fs::copy(from, to)?;
        // Build tools tell what is up to date by it.
        File::open(to)?.set_modified(meta.modified()?)?;
    }
    // Sockets and pipes have nothing to copy.
    Ok(())
}

/// Removes whatever is at `path`, without following a symlink.
fn remove(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "symlinks can't be copied here",
    ))
}
