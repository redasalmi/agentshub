//! Copying a project's working tree into its new worktree as it is, with the
//! changes nobody committed and the files git ignores, like `.env` and
//! `node_modules`, so the agent can build and test there right away.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

/// How much of a file is copied between checks of whether to stop, so a
/// large one can't keep a stopped copy going for long.
pub(super) const CHUNK: u64 = 32 << 20;

/// Makes `dst`, a new checkout of `src`'s repository, hold what `src`
/// holds, down to file modes and modification times, unless `stopped` says
/// to stop. Each one's `.git` stays its own. `skip`, where the worktrees
/// are kept, isn't copied, for a repository that holds it.
///
/// What goes from `src` while it is copied, as a build or a dev server
/// makes it do, goes from `dst` too. What can't be read is left out, and
/// returned, relative to `src`.
pub(super) fn mirror(
    src: &Path,
    dst: &Path,
    skip: &Path,
    stopped: &dyn Fn() -> bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let names = list(src).with_context(|| format!("Couldn't read {}", src.display()))?;
    let mut mirror = Mirror {
        root: src,
        skip,
        stopped,
        unreadable: Vec::new(),
    };
    mirror.dir(src, dst, names, true)?;
    Ok(mirror.unreadable)
}

struct Mirror<'a> {
    root: &'a Path,
    skip: &'a Path,
    stopped: &'a dyn Fn() -> bool,
    unreadable: Vec<PathBuf>,
}

impl Mirror<'_> {
    fn dir(
        &mut self,
        src: &Path,
        dst: &Path,
        names: HashSet<OsString>,
        top: bool,
    ) -> anyhow::Result<()> {
        // What the checkout has that the project doesn't, as a file deleted
        // without a commit.
        for entry in
            fs::read_dir(dst).with_context(|| format!("Couldn't read {}", dst.display()))?
        {
            let name = entry?.file_name();
            if name != ".git" && !names.contains(&name) {
                let path = dst.join(name);
                remove(&path).with_context(|| format!("Couldn't remove {}", path.display()))?;
            }
        }
        for name in names {
            self.check()?;
            let (from, to) = (src.join(&name), dst.join(&name));
            if from == self.skip {
                continue;
            }
            let meta = match fs::symlink_metadata(&from) {
                Ok(meta) => meta,
                Err(err) => {
                    self.missed(&from, &to, err)?;
                    continue;
                }
            };
            if name == ".git" && (top || !meta.is_dir()) {
                continue;
            }
            self.entry(&from, &to, &meta)?;
        }
        Ok(())
    }

    fn entry(&mut self, from: &Path, to: &Path, meta: &Metadata) -> anyhow::Result<()> {
        let failed = || format!("Couldn't copy {}", from.display());
        let kind = meta.file_type();
        if kind.is_dir() {
            // A Python virtualenv names the folder it was made in, in its
            // scripts and the paths it imports from, so a copy would run the
            // project's code, and install into its environment. The agent
            // makes one of its own.
            if from.join("pyvenv.cfg").is_file() {
                return Ok(());
            }
            // A `.git` file ties a submodule, or another worktree, to its
            // repository. Copied without it, git in the worktree wouldn't see
            // the agent's changes in it, and removing the worktree would lose
            // them; copied with it, git would work on that repository. It's
            // left as git checks it out: a submodule empty, the rest absent.
            if from.join(".git").is_file() {
                return Ok(());
            }
            let names = match list(from) {
                Ok(names) => names,
                Err(err) => return self.missed(from, to, err),
            };
            if !fs::symlink_metadata(to).is_ok_and(|meta| meta.is_dir()) {
                remove(to)
                    .and_then(|()| fs::create_dir(to))
                    .with_context(failed)?;
            }
            self.dir(from, to, names, false)?;
            // Last, as a folder that can't be written to is.
            fs::set_permissions(to, meta.permissions()).with_context(failed)?;
        } else if kind.is_symlink() {
            let target = match fs::read_link(from) {
                Ok(target) => target,
                Err(err) => return self.missed(from, to, err),
            };
            remove(to)
                .and_then(|()| symlink(&target, to))
                .with_context(failed)?;
        } else if kind.is_file() {
            let source = match File::open(from) {
                Ok(source) => source,
                Err(err) => return self.missed(from, to, err),
            };
            remove(to).with_context(failed)?;
            self.copy_file(source, to, meta).with_context(failed)?;
        }
        // Sockets and pipes have nothing to copy.
        Ok(())
    }

    /// Copies `source` to the new file `to` a chunk at a time. Each chunk is
    /// a clone where the filesystem can, as btrfs and XFS do, so a large
    /// `node_modules` takes no room of its own until it changes.
    fn copy_file(&self, mut source: File, to: &Path, meta: &Metadata) -> anyhow::Result<()> {
        let mut copy = File::options().write(true).create_new(true).open(to)?;
        loop {
            self.check()?;
            if std::io::copy(&mut (&mut source).take(CHUNK), &mut copy)? < CHUNK {
                break;
            }
        }
        copy.set_permissions(meta.permissions())?;
        // Build tools tell what is up to date by it.
        copy.set_modified(meta.modified()?)?;
        Ok(())
    }

    /// Takes in that `from` went, or can't be read, when its turn came: what
    /// went goes from `to` too, and what can't be read is noted.
    fn missed(&mut self, from: &Path, to: &Path, err: std::io::Error) -> anyhow::Result<()> {
        match err.kind() {
            ErrorKind::NotFound => {
                remove(to).with_context(|| format!("Couldn't remove {}", to.display()))
            }
            ErrorKind::PermissionDenied => {
                let path = from.strip_prefix(self.root).unwrap_or(from);
                self.unreadable.push(path.to_path_buf());
                Ok(())
            }
            _ => Err(anyhow::Error::new(err).context(format!("Couldn't read {}", from.display()))),
        }
    }

    fn check(&self) -> anyhow::Result<()> {
        if (self.stopped)() {
            bail!("The worktree was abandoned.");
        }
        Ok(())
    }
}

fn list(dir: &Path) -> std::io::Result<HashSet<OsString>> {
    fs::read_dir(dir)?
        .map(|entry| Ok(entry?.file_name()))
        .collect()
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
