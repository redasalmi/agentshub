//! Git worktrees that chats run in, so agents working in the same project
//! don't edit each other's files. Agents Hub makes them with git itself, which
//! works the same for every agent, copies the project's working tree in as it
//! is (see `mirror`), and keeps them in its data folder, out of the project.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use smol::future::FutureExt as _;

mod mirror;
mod remove;

pub use remove::{Removal, remove};

/// The branch prefix of the worktrees Agents Hub makes.
const BRANCH_PREFIX: &str = "agentshub/";

/// How long git and its hooks get to stop checking out a worktree when
/// Agents Hub quits, or its chat is closed, before they're killed.
const STOP_GRACE: Duration = Duration::from_millis(500);

/// How long git gets to say which repository a folder is in, as a folder on a
/// network filesystem that stopped answering can keep it waiting.
const REPO_TIMEOUT: Duration = Duration::from_secs(2);

/// Where a project folder sits in its git repository.
#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    /// The repository's working tree.
    pub root: PathBuf,
    /// The project folder's path under `root`, empty when it is `root`.
    pub prefix: PathBuf,
}

/// The repository `dir` is in, if git is installed and it is in one with a
/// working tree. Fails if git doesn't answer in time, which doesn't tell
/// whether it is in one.
pub fn repo(dir: &Path) -> anyhow::Result<Option<Repo>> {
    let Ok(mut child) = git(dir)
        .args(["rev-parse", "--show-toplevel", "--show-prefix"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Ok(None);
    };
    let deadline = Instant::now() + REPO_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                child.kill().ok();
                child.wait().ok();
                bail!(
                    "Couldn't tell whether {} is in a git repository: git didn't answer in time.",
                    dir.display()
                );
            }
        }
    };
    let mut stdout = String::new();
    if !status.success()
        || child
            .stdout
            .take()
            .is_none_or(|mut out| out.read_to_string(&mut stdout).is_err())
    {
        return Ok(None);
    }
    let mut lines = stdout.lines();
    let Some(root) = lines.next().filter(|root| !root.is_empty()) else {
        return Ok(None);
    };
    let prefix = PathBuf::from(lines.next().unwrap_or_default());
    Ok(Some(Repo {
        root: PathBuf::from(root),
        prefix,
    }))
}

/// A worktree Agents Hub made for a chat.
#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    /// Its working tree, which removing it deletes.
    pub root: PathBuf,
    /// The project's folder in it, which the chat runs in. It is `root` for
    /// a project at the top of its repository. An agent can delete or rename
    /// it, so it says nothing about whether the worktree is still there.
    pub folder: PathBuf,
}

impl Worktree {
    /// The branch Agents Hub checked the worktree out on.
    pub fn branch(&self) -> String {
        branch_of(&self.root)
    }
}

/// What a checkout made.
#[derive(Debug)]
pub struct Created {
    pub worktree: Worktree,
    /// What in the project couldn't be read, so isn't in the worktree,
    /// relative to the repository's working tree.
    pub unreadable: Vec<PathBuf>,
}

/// A worktree being checked out for a new chat, on a thread of its own,
/// since that takes a while in a large repository. What it makes is taken
/// once: by the chat, through `created`, or by `abandon`.
pub struct Checkout {
    project: PathBuf,
    git: Arc<Mutex<Git>>,
    created: smol::channel::Receiver<anyhow::Result<Created>>,
}

/// The git checking a worktree out.
#[derive(Default)]
struct Git {
    /// Whether the checkout was abandoned, so git doesn't start anymore.
    stopped: bool,
    /// Git, until it is reaped once it and the hooks it ran are done. It
    /// leads a process group of its own, with those hooks.
    child: Option<Child>,
}

impl Checkout {
    /// Starts making a worktree of `project`'s repository on a new branch
    /// from its current commit, holding what its working tree holds.
    pub fn start(project: &Path) -> Self {
        Self::start_in(project, worktrees_dir())
    }

    /// The same, keeping the worktree in `dir`.
    fn start_in(project: &Path, dir: Option<PathBuf>) -> Self {
        let git = Arc::<Mutex<Git>>::default();
        let (tx, created) = smol::channel::bounded(1);
        thread::spawn({
            let (project, git) = (project.to_path_buf(), Arc::clone(&git));
            move || {
                let created = dir
                    .context("There is no home folder to keep the worktree in.")
                    .and_then(|dir| create_in(&project, &dir, &git));
                tx.send_blocking(created).ok();
            }
        });
        Self {
            project: project.to_path_buf(),
            git,
            created,
        }
    }

    /// What the checkout made, once git is done, or an error once it was
    /// taken.
    pub fn created(&self) -> smol::channel::Receiver<anyhow::Result<Created>> {
        self.created.clone()
    }

    /// Whether what it made was taken.
    pub fn is_taken(&self) -> bool {
        self.created.is_closed() && self.created.is_empty()
    }

    /// Has git stop, as when its chat isn't wanted anymore. A checkout git
    /// was too far along to stop still sends what it made, which then has to
    /// be removed; one it stopped removes what it made and sends an error.
    /// Git and its hooks get `STOP_GRACE` to stop before they're killed, so
    /// a hook that ignores SIGTERM can't keep the checkout going.
    pub fn cancel(&self) {
        if !stop(&self.git, false) {
            return;
        }
        let git = Arc::clone(&self.git);
        thread::spawn(move || {
            thread::sleep(STOP_GRACE);
            stop(&git, true);
        });
    }
}

/// Has git stop: SIGTERM lets it remove a checkout it hasn't finished, and
/// SIGKILL is for when it or a hook doesn't stop. Returns whether git was
/// still running.
fn stop(git: &Mutex<Git>, kill: bool) -> bool {
    let mut git = lock(git);
    git.stopped = true;
    let Some(child) = &mut git.child else {
        return false;
    };
    #[cfg(unix)]
    {
        let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
        // Git isn't reaped while it is held here, so its process group is
        // still its own.
        // SAFETY: killpg takes no pointers.
        unsafe { libc::killpg(child.id() as libc::pid_t, signal) };
    }
    #[cfg(not(unix))]
    if kill {
        child.kill().ok();
    }
    true
}

/// Stops the checkouts still under way, as when Agents Hub quits, and
/// removes the worktrees they made, which no chat would remove otherwise.
/// Git and its hooks get `STOP_GRACE` to stop before they're killed.
pub fn abandon(checkouts: Vec<Checkout>) {
    for checkout in &checkouts {
        stop(&checkout.git, false);
    }
    let deadline = Instant::now() + STOP_GRACE;
    smol::block_on(async {
        for checkout in checkouts {
            let created = async { Some(checkout.created.recv().await) }
                .or(async {
                    smol::Timer::at(deadline).await;
                    None
                })
                .await;
            let created = match created {
                Some(created) => created,
                None => {
                    stop(&checkout.git, true);
                    checkout.created.recv().await
                }
            };
            // A checkout that failed or was stopped removed what it made.
            if let Ok(Ok(Created { worktree, .. })) = created {
                let branch = branch_of(&worktree.root);
                let left = discard(&checkout.project, &worktree.root, &branch);
                if !left.is_empty() {
                    eprintln!(
                        "can't remove the abandoned worktree {}: {}",
                        worktree.root.display(),
                        left.join(" and ")
                    );
                }
            }
        }
    });
}

/// Makes a worktree of `project`'s repository in `dir`, unless the
/// checkout `state` is for is stopped first.
fn create_in(project: &Path, dir: &Path, state: &Mutex<Git>) -> anyhow::Result<Created> {
    let repo = repo(project)?.context("The project isn't in a git repository.")?;
    let parent = dir.join(project_dir_name(&repo.root));
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("Couldn't create {}", parent.display()))?;
    // A name nothing else has, for both the folder and the branch.
    let (name, path, branch) = loop {
        let name = format!("{:08x}", uuid::Uuid::new_v4().as_fields().0);
        let path = parent.join(&name);
        let branch = branch_of(&path);
        if !path.exists() && !branch_exists(&repo.root, &branch) {
            break (name, path, branch);
        }
    };
    let mut command = git(&repo.root);
    command
        .args(["worktree", "add", "-b", &branch])
        .arg(&path)
        .arg("HEAD");
    let (status, stderr) = run_checkout(command, state)?;
    let copied = if status.success() {
        let skip = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        mirror::mirror(&repo.root, &path, &skip, &|| lock(state).stopped).map_err(|err| {
            (
                format!("Couldn't copy the project into the worktree {name}"),
                format!("{err:#}"),
            )
        })
    } else {
        Err((
            format!("git couldn't create the worktree {name}"),
            stderr.trim().to_string(),
        ))
    };
    let unreadable = match copied {
        Ok(unreadable) => unreadable,
        Err((what, why)) => {
            // Git can fail after making the branch and the checkout, as when a
            // post-checkout hook fails or it is stopped, and leaves them behind,
            // as a copy that fails does. No chat would refer to them, so they go
            // again.
            let left = discard(&repo.root, &path, &branch);
            if left.is_empty() {
                bail!("{what}: {why}");
            }
            bail!("{what}, and {}: {why}", left.join(" and "));
        }
    };
    Ok(Created {
        worktree: Worktree {
            folder: path.join(&repo.prefix),
            root: path,
        },
        unreadable,
    })
}

/// Runs the git `command` that checks out a worktree, in a process group of
/// its own so that stopping it stops its hooks too, unless `state` is
/// stopped first. Returns how it exited and what it wrote to stderr.
fn run_checkout(mut command: Command, state: &Mutex<Git>) -> anyhow::Result<(ExitStatus, String)> {
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let stderr = {
        let mut git = lock(state);
        if git.stopped {
            bail!("The worktree was abandoned.");
        }
        let mut child = command.spawn().context("Couldn't run git")?;
        let stderr = child.stderr.take();
        git.child = Some(child);
        stderr
    };
    // Git's stderr closes once it and the hooks it ran that kept it are
    // gone. Git isn't reaped before, so its process group stays its own
    // until then, for stopping a hook that outlives it.
    let mut read = Vec::new();
    if let Some(mut stderr) = stderr {
        stderr.read_to_end(&mut read).ok();
    }
    let mut child = lock(state).child.take().context("git was lost")?;
    let status = child.wait().context("Couldn't wait for git")?;
    Ok((status, String::from_utf8_lossy(&read).into_owned()))
}

/// Removes a worktree no chat ran in, and its branch. Its checkout is brand
/// new, so it goes even with changes, as a hook may make, or locked, as git
/// leaves one it was killed checking out. The branch only goes while HEAD
/// has its commits. Returns what is left of them.
fn discard(repo: &Path, path: &Path, branch: &str) -> Vec<String> {
    if path.exists() {
        git(repo)
            .args(["worktree", "remove", "--force", "--force"])
            .arg(path)
            .output()
            .ok();
    }
    if branch_exists(repo, branch) {
        git(repo)
            .args(["branch", "--quiet", "-d", branch])
            .output()
            .ok();
    }
    let mut left = Vec::new();
    if path.exists() {
        left.push(format!("its checkout is left at {}", path.display()));
    }
    if branch_exists(repo, branch) {
        left.push(format!("its branch {branch} is left"));
    }
    left
}

/// The branch of the worktree at `root`, named after its folder.
fn branch_of(root: &Path) -> String {
    format!(
        "{BRANCH_PREFIX}{}",
        root.file_name().unwrap_or_default().to_string_lossy()
    )
}

fn lock(git: &Mutex<Git>) -> MutexGuard<'_, Git> {
    git.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the worktrees live: beside the database, in `worktrees`.
fn worktrees_dir() -> Option<PathBuf> {
    Some(crate::store::path()?.parent()?.join("worktrees"))
}

/// The folder a repository's worktrees go in, named after it.
pub(super) fn project_dir_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".into())
}

pub(super) fn branch_exists(root: &Path, branch: &str) -> bool {
    git(root)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .status()
        .is_ok_and(|status| status.success())
}

/// Git run in `dir`, without a terminal to ask anything in.
pub(super) fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = git(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository with a commit, and the data folder worktrees go in.
    fn setup() -> (PathBuf, PathBuf) {
        let base = temp_dir();
        let repo = base.join("repo");
        fs::create_dir_all(repo.join("app")).unwrap();
        fs::write(repo.join("app/main.rs"), "fn main() {}").unwrap();
        run(&repo, &["init", "-q"]);
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        (base, repo)
    }

    #[test]
    fn finds_where_a_folder_sits_in_its_repository() {
        let (base, root) = setup();
        assert_eq!(
            repo(&root).unwrap(),
            Some(Repo {
                root: root.clone(),
                prefix: PathBuf::new()
            })
        );
        assert_eq!(
            repo(&root.join("app")).unwrap(),
            Some(Repo {
                root: root.clone(),
                prefix: PathBuf::from("app")
            })
        );
        let outside = base.join("outside");
        fs::create_dir(&outside).unwrap();
        // The temp folder may itself be in a repository on some machines.
        if repo(&base).unwrap().is_none() {
            assert_eq!(repo(&outside).unwrap(), None);
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn creates_and_removes_a_worktree_unless_it_has_changes() {
        let (base, root) = setup();
        let worktree = create_in(
            &root.join("app"),
            &base.join("worktrees"),
            &Mutex::default(),
        )
        .unwrap()
        .worktree;
        assert_eq!(worktree.folder, worktree.root.join("app"));
        assert!(worktree.root.starts_with(base.join("worktrees/repo")));
        assert!(worktree.folder.join("main.rs").exists());
        let branch = worktree.branch();
        assert!(branch_exists(&root, &branch));

        fs::write(worktree.folder.join("new.rs"), "").unwrap();
        assert!(matches!(
            remove(&worktree, &root).unwrap(),
            Removal::Kept(_)
        ));
        assert!(worktree.folder.exists());

        fs::remove_file(worktree.folder.join("new.rs")).unwrap();
        assert_eq!(remove(&worktree, &root).unwrap(), Removal::Removed);
        assert!(!worktree.root.exists());
        // What was committed on it stays.
        assert!(branch_exists(&root, &branch));
        fs::remove_dir_all(base).unwrap();
    }

    fn rev(dir: &Path, rev: &str) -> String {
        let output = git(dir).args(["rev-parse", rev]).output().unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn keeps_commits_made_on_a_detached_head_on_a_branch() {
        let (base, root) = setup();
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        run(&worktree.root, &["checkout", "-q", "--detach"]);
        fs::write(worktree.root.join("detached.rs"), "").unwrap();
        run(&worktree.root, &["add", "."]);
        run(&worktree.root, &["commit", "-q", "-m", "detached"]);
        let head = rev(&worktree.root, "HEAD");

        let Removal::Rescued(branch) = remove(&worktree, &root).unwrap() else {
            panic!("the detached commit wasn't rescued");
        };
        assert!(!worktree.root.exists());
        assert_eq!(rev(&root, &branch), head);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_detached_head_a_branch_has_needs_no_rescue() {
        let (base, root) = setup();
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        run(&worktree.root, &["checkout", "-q", "--detach"]);
        assert_eq!(remove(&worktree, &root).unwrap(), Removal::Removed);
        let output = git(&root)
            .args(["branch", "--list", "*-detached*"])
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_kept_worktree_gets_no_rescue_branch() {
        let (base, root) = setup();
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        run(&worktree.root, &["checkout", "-q", "--detach"]);
        fs::write(worktree.root.join("detached.rs"), "").unwrap();
        run(&worktree.root, &["add", "."]);
        run(&worktree.root, &["commit", "-q", "-m", "detached"]);
        fs::write(worktree.root.join("uncommitted.rs"), "").unwrap();
        assert!(matches!(
            remove(&worktree, &root).unwrap(),
            Removal::Kept(_)
        ));
        assert!(worktree.root.exists());
        let output = git(&root)
            .args(["branch", "--list", "*-detached*"])
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn removes_a_worktree_whose_project_folder_an_agent_deleted() {
        let (base, root) = setup();
        let worktree = create_in(
            &root.join("app"),
            &base.join("worktrees"),
            &Mutex::default(),
        )
        .unwrap()
        .worktree;
        run(&worktree.root, &["rm", "-q", "-r", "app"]);
        run(&worktree.root, &["commit", "-q", "-m", "drop app"]);
        assert!(!worktree.folder.exists());
        assert_eq!(remove(&worktree, &root).unwrap(), Removal::Removed);
        assert!(!worktree.root.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn forgets_a_worktree_deleted_by_hand() {
        let (base, root) = setup();
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        fs::remove_dir_all(&worktree.root).unwrap();
        assert_eq!(remove(&worktree, &root).unwrap(), Removal::Removed);
        let output = git(&root).args(["worktree", "list"]).output().unwrap();
        let list = String::from_utf8(output.stdout).unwrap();
        assert!(!list.contains(&*worktree.root.to_string_lossy()));
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn leaves_nothing_behind_when_a_hook_fails() {
        let (base, root) = setup();
        hook(&root, "#!/bin/sh\necho hook failed >&2\nexit 1\n");
        let dir = base.join("worktrees");
        let err = create_in(&root, &dir, &Mutex::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("hook failed"), "{err}");
        assert!(!err.contains("left"), "{err}");
        assert_eq!(fs::read_dir(dir.join("repo")).unwrap().count(), 0);
        let output = git(&root)
            .args(["branch", "--list", &format!("{BRANCH_PREFIX}*")])
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    fn hook(root: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        let hook = root.join(".git/hooks/post-checkout");
        fs::write(&hook, script).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Asserts that the repository has no worktree or branch of Agents Hub.
    fn assert_nothing_left(root: &Path, dir: &Path) {
        let left = fs::read_dir(dir.join("repo")).map_or(0, |dir| dir.count());
        assert_eq!(left, 0, "a checkout is left");
        let output = git(root)
            .args(["branch", "--list", &format!("{BRANCH_PREFIX}*")])
            .output()
            .unwrap();
        assert!(output.stdout.is_empty(), "a branch is left");
        let output = git(root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let listed = String::from_utf8(output.stdout).unwrap();
        assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
    }

    /// Abandons a checkout while its post-checkout hook runs `script`.
    #[cfg(unix)]
    fn abandon_during_hook(script: &str) {
        let (base, root) = setup();
        let started = base.join("started");
        hook(
            &root,
            &format!("#!/bin/sh\ntouch '{}'\n{script}\n", started.display()),
        );
        let dir = base.join("worktrees");
        let checkout = Checkout::start_in(&root, Some(dir.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(Instant::now() < deadline, "the hook didn't start");
            thread::sleep(Duration::from_millis(10));
        }
        let begun = Instant::now();
        abandon(vec![checkout]);
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert_nothing_left(&root, &dir);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_checkout_stops_git_and_removes_it() {
        let (base, root) = setup();
        let started = base.join("started");
        hook(
            &root,
            &format!("#!/bin/sh\ntouch '{}'\nsleep 30\n", started.display()),
        );
        let dir = base.join("worktrees");
        let checkout = Checkout::start_in(&root, Some(dir.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(Instant::now() < deadline, "the hook didn't start");
            thread::sleep(Duration::from_millis(10));
        }
        let begun = Instant::now();
        checkout.cancel();
        assert!(checkout.created().recv_blocking().unwrap().is_err());
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert_nothing_left(&root, &dir);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_checkout_kills_a_hook_ignoring_sigterm() {
        let (base, root) = setup();
        let started = base.join("started");
        hook(
            &root,
            &format!(
                "#!/bin/sh\ntrap '' TERM\ntouch '{}'\nsleep 30\n",
                started.display()
            ),
        );
        let dir = base.join("worktrees");
        let checkout = Checkout::start_in(&root, Some(dir.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(Instant::now() < deadline, "the hook didn't start");
            thread::sleep(Duration::from_millis(10));
        }
        let begun = Instant::now();
        checkout.cancel();
        assert!(checkout.created().recv_blocking().unwrap().is_err());
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert_nothing_left(&root, &dir);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn abandoning_a_checkout_stops_git_and_removes_it() {
        abandon_during_hook("sleep 30");
    }

    #[cfg(unix)]
    #[test]
    fn abandoning_a_checkout_kills_a_hook_that_ignores_sigterm() {
        abandon_during_hook("trap '' TERM\nsleep 30");
    }

    #[test]
    fn abandoning_removes_a_checkout_nobody_took() {
        let (base, root) = setup();
        let dir = base.join("worktrees");
        let checkout = Checkout::start_in(&root, Some(dir.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while checkout.created.is_empty() {
            assert!(Instant::now() < deadline, "the checkout didn't finish");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!checkout.is_taken());
        abandon(vec![checkout]);
        assert_nothing_left(&root, &dir);
        fs::remove_dir_all(base).unwrap();
    }

    /// What a folder holds, but for its `.git`: each path, with its kind,
    /// mode and, for a file, its bytes and modification time.
    #[cfg(unix)]
    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
        use std::os::unix::fs::PermissionsExt as _;

        fn walk(root: &Path, dir: &Path, into: &mut std::collections::BTreeMap<PathBuf, String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if relative == Path::new(".git") {
                    continue;
                }
                let meta = fs::symlink_metadata(&path).unwrap();
                let mode = meta.permissions().mode() & 0o7777;
                let what = if meta.is_symlink() {
                    format!("link -> {}", fs::read_link(&path).unwrap().display())
                } else if meta.is_dir() {
                    walk(root, &path, into);
                    format!("dir {mode:o}")
                } else {
                    format!(
                        "file {mode:o} {:?} {:?}",
                        meta.modified().unwrap(),
                        fs::read(&path).unwrap()
                    )
                };
                into.insert(relative, what);
            }
        }
        let mut tree = std::collections::BTreeMap::new();
        walk(root, root, &mut tree);
        tree
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copies_the_working_tree_as_it_is() {
        use std::os::unix::fs::symlink;

        let (base, root) = setup();
        fs::write(root.join(".gitignore"), "/.env\nnode_modules/\n").unwrap();
        fs::write(root.join("app/gone.rs"), "gone").unwrap();
        fs::write(root.join("app/kept.rs"), "kept").unwrap();
        fs::write(root.join("run.sh"), "#!/bin/sh\n").unwrap();
        chmod(&root.join("run.sh"), 0o755);
        symlink("app/main.rs", root.join("link")).unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "more"]);

        // Changes nobody committed, of every kind.
        fs::write(root.join("app/main.rs"), "fn main() { changed() }").unwrap();
        fs::remove_file(root.join("app/gone.rs")).unwrap();
        fs::write(root.join("app/staged.rs"), "staged").unwrap();
        run(&root, &["add", "app/staged.rs"]);
        fs::remove_file(root.join("link")).unwrap();
        fs::write(root.join("link"), "a file now").unwrap();
        fs::write(root.join("notes.md"), "untracked").unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        // What git ignores.
        fs::write(root.join(".env"), "TOKEN=1").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "module.exports = 1").unwrap();
        chmod(&root.join("node_modules/pkg/index.js"), 0o755);
        symlink("../pkg/index.js", root.join("node_modules/.bin/tool")).unwrap();
        fs::write(root.join("node_modules/pkg/readonly"), "r").unwrap();
        chmod(&root.join("node_modules/pkg/readonly"), 0o444);
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(root.join("app/kept.rs"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        // A submodule's or another worktree's tie to its repository.
        fs::create_dir_all(root.join("vendor/sub")).unwrap();
        fs::write(root.join("vendor/sub/.git"), "gitdir: /elsewhere").unwrap();
        fs::write(root.join("vendor/sub/lib.rs"), "lib").unwrap();

        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;

        let mut expected = tree(&root);
        for path in ["vendor/sub", "vendor/sub/.git", "vendor/sub/lib.rs"] {
            expected.remove(Path::new(path));
        }
        assert_eq!(tree(&worktree.root), expected);
        assert_eq!(rev(&worktree.root, "HEAD"), rev(&root, "HEAD"));
        let output = git(&worktree.root)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let status = String::from_utf8(output.stdout).unwrap();
        for change in [
            " M app/main.rs",
            " D app/gone.rs",
            " T link",
            "?? app/staged.rs",
            "?? notes.md",
        ] {
            assert!(
                status.lines().any(|line| line == change),
                "{change} in {status}"
            );
        }
        assert!(
            !status.contains("node_modules") && !status.contains(".env"),
            "{status}"
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_worktree_with_only_ignored_files_is_still_removed() {
        let (base, root) = setup();
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "ignore"]);
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "").unwrap();
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        assert!(worktree.root.join("node_modules/pkg/index.js").exists());
        assert_eq!(remove(&worktree, &root).unwrap(), Removal::Removed);
        assert!(!worktree.root.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_repository_holding_the_worktrees_isnt_copied_into_them() {
        let (base, root) = setup();
        let dir = root.join("worktrees");
        let worktree = create_in(&root, &dir, &Mutex::default()).unwrap().worktree;
        assert!(worktree.root.join("app/main.rs").exists());
        assert!(!worktree.root.join("worktrees").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn leaves_out_what_cant_be_read() {
        let (base, root) = setup();
        fs::write(root.join("secret"), "").unwrap();
        fs::create_dir(root.join("pgdata")).unwrap();
        fs::write(root.join("pgdata/table"), "").unwrap();
        chmod(&root.join("secret"), 0o000);
        chmod(&root.join("pgdata"), 0o000);
        // Root reads them anyway.
        if File::open(root.join("secret")).is_ok() {
            chmod(&root.join("pgdata"), 0o755);
            fs::remove_dir_all(base).unwrap();
            return;
        }
        let created = create_in(&root, &base.join("worktrees"), &Mutex::default()).unwrap();
        let mut unreadable = created.unreadable.clone();
        unreadable.sort();
        assert_eq!(
            unreadable,
            [PathBuf::from("pgdata"), PathBuf::from("secret")]
        );
        let worktree = created.worktree;
        assert!(worktree.folder.join("app/main.rs").exists());
        assert!(!worktree.root.join("secret").exists());
        assert!(!worktree.root.join("pgdata").exists());
        chmod(&root.join("pgdata"), 0o755);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn what_goes_during_the_copy_goes_from_the_worktree_too() {
        let (base, root) = setup();
        fs::write(root.join("cache.rs"), "").unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "cache"]);
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/out"), "").unwrap();
        let dst = base.join("copy");
        run(
            &root,
            &["worktree", "add", "-q", "--detach", dst.to_str().unwrap()],
        );
        let first = std::cell::Cell::new(true);
        let unreadable = mirror::mirror(&root, &dst, &base.join("worktrees"), &|| {
            if first.replace(false) {
                fs::remove_file(root.join("cache.rs")).unwrap();
                fs::remove_dir_all(root.join("target")).unwrap();
            }
            false
        })
        .unwrap();
        assert!(unreadable.is_empty());
        assert!(dst.join("app/main.rs").exists());
        assert!(!dst.join("cache.rs").exists());
        assert!(!dst.join("target").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn leaves_submodules_empty_so_no_change_in_them_goes_unseen() {
        let (base, root) = setup();
        let lib = base.join("lib");
        fs::create_dir(&lib).unwrap();
        fs::write(lib.join("lib.rs"), "v1").unwrap();
        run(&lib, &["init", "-q"]);
        run(&lib, &["add", "."]);
        run(&lib, &["commit", "-q", "-m", "lib"]);
        run(
            &root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                lib.to_str().unwrap(),
                "lib",
            ],
        );
        run(&root, &["commit", "-q", "-m", "submodule"]);
        fs::write(root.join("lib/lib.rs"), "changed in the project").unwrap();

        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        assert!(worktree.root.join("lib").is_dir());
        assert_eq!(fs::read_dir(worktree.root.join("lib")).unwrap().count(), 0);
        assert!(worktree.root.join(".gitmodules").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn leaves_python_virtualenvs_out() {
        let (base, root) = setup();
        fs::create_dir_all(root.join(".venv/bin")).unwrap();
        fs::write(root.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::os::unix::fs::symlink("/usr/bin/python3", root.join(".venv/bin/python")).unwrap();
        fs::write(root.join(".gitignore"), ".venv/\n").unwrap();
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default())
            .unwrap()
            .worktree;
        assert!(worktree.root.join(".gitignore").exists());
        assert!(!worktree.root.join(".venv").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stops_in_the_middle_of_a_large_file() {
        let (base, root) = setup();
        File::create(root.join("large"))
            .unwrap()
            .set_len(3 * mirror::CHUNK)
            .unwrap();
        let dst = base.join("copy");
        fs::create_dir(&dst).unwrap();
        let copied = || fs::metadata(dst.join("large")).map_or(0, |meta| meta.len());
        let err =
            mirror::mirror(&root, &dst, &base.join("worktrees"), &|| copied() > 0).unwrap_err();
        assert_eq!(err.root_cause().to_string(), "The worktree was abandoned.");
        assert_eq!(copied(), mirror::CHUNK);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stops_copying_once_stopped() {
        let (base, root) = setup();
        let dst = base.join("copy");
        fs::create_dir(&dst).unwrap();
        let err = mirror::mirror(&root, &dst, &base.join("worktrees"), &|| true).unwrap_err();
        assert_eq!(err.to_string(), "The worktree was abandoned.");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn abandoning_keeps_a_checkout_its_chat_took() {
        let (base, root) = setup();
        let checkout = Checkout::start_in(&root, Some(base.join("worktrees")));
        let worktree = checkout
            .created()
            .recv_blocking()
            .unwrap()
            .unwrap()
            .worktree;
        // The checkout's thread drops its end of the channel just after
        // sending, which is what makes the checkout count as taken.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !checkout.created.is_closed() {
            assert!(
                Instant::now() < deadline,
                "the checkout's thread didn't end"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(checkout.is_taken());
        abandon(vec![checkout]);
        assert!(worktree.root.exists());
        assert!(branch_exists(&root, &branch_of(&worktree.root)));
        fs::remove_dir_all(base).unwrap();
    }
}
