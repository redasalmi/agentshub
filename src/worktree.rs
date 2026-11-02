//! Git worktrees that chats run in, so agents working in the same project
//! don't edit each other's files. Agents Hub makes them with git itself, which
//! works the same for every agent, and keeps them in its data folder, out of
//! the project.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use smol::future::FutureExt as _;

/// The branch prefix of the worktrees Agents Hub makes.
const BRANCH_PREFIX: &str = "agentshub/";

/// How long git and its hooks get to stop checking out a worktree when
/// Agents Hub quits, or its chat is closed, before they're killed.
const STOP_GRACE: Duration = Duration::from_millis(500);

/// Where a project folder sits in its git repository.
#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    /// The repository's working tree.
    pub root: PathBuf,
    /// The project folder's path under `root`, empty when it is `root`.
    pub prefix: PathBuf,
}

/// The repository `dir` is in, if git is installed and it is in one with a
/// working tree.
pub fn repo(dir: &Path) -> Option<Repo> {
    let output = git(dir)
        .args(["rev-parse", "--show-toplevel", "--show-prefix"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut lines = stdout.lines();
    let root = PathBuf::from(lines.next().filter(|root| !root.is_empty())?);
    let prefix = PathBuf::from(lines.next().unwrap_or_default());
    Some(Repo { root, prefix })
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

/// A worktree being checked out for a new chat, on a thread of its own,
/// since that takes a while in a large repository. What it makes is taken
/// once: by the chat, through `created`, or by `abandon`.
pub struct Checkout {
    project: PathBuf,
    git: Arc<Mutex<Git>>,
    created: smol::channel::Receiver<anyhow::Result<Worktree>>,
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
    /// from its current commit.
    pub fn start(project: &Path) -> Self {
        Self::start_in(project, worktrees_dir())
    }

    /// The same, keeping the worktree in `dir`.
    fn start_in(project: &Path, dir: Option<PathBuf>) -> Self {
        let git = Arc::<Mutex<Git>>::default();
        let (tx, created) = smol::channel::bounded(1);
        thread::spawn({
            let (project, git) = (project.to_path_buf(), git.clone());
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
    pub fn created(&self) -> smol::channel::Receiver<anyhow::Result<Worktree>> {
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
        let git = self.git.clone();
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
            if let Ok(Ok(worktree)) = created {
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
fn create_in(project: &Path, dir: &Path, state: &Mutex<Git>) -> anyhow::Result<Worktree> {
    let repo = repo(project).context("The project isn't in a git repository.")?;
    let parent = dir.join(project_dir_name(&repo.root));
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("Couldn't create {}", parent.display()))?;
    // A name nothing else has, for both the folder and the branch.
    let (name, path, branch) = loop {
        let name = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
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
    if !status.success() {
        let why = stderr.trim().to_string();
        // Git can fail after making the branch and the checkout, as when a
        // post-checkout hook fails or it is stopped, and leaves them behind.
        // No chat would refer to them, so they go again.
        let left = discard(&repo.root, &path, &branch);
        if left.is_empty() {
            bail!("git couldn't create the worktree {name}: {why}");
        }
        bail!(
            "git couldn't create the worktree {name}, and {}: {why}",
            left.join(" and ")
        );
    }
    Ok(Worktree {
        folder: path.join(&repo.prefix),
        root: path,
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
    if repo(root).is_none_or(|repo| repo.root != canonical) {
        return Ok(Removal::Kept(format!(
            "{} isn't a git worktree anymore",
            root.display()
        )));
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
    let short = &head[..head.len().min(8)];
    let branch = [
        format!("{BRANCH_PREFIX}{name}-detached"),
        format!("{BRANCH_PREFIX}{name}-detached-{short}"),
    ]
    .into_iter()
    .find(|branch| !branch_exists(root, branch))
    .unwrap_or_else(|| {
        format!(
            "{BRANCH_PREFIX}{name}-detached-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
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

/// Where the worktrees live: beside the database, in `worktrees`.
fn worktrees_dir() -> Option<PathBuf> {
    Some(crate::db::path()?.parent()?.join("worktrees"))
}

/// The folder a repository's worktrees go in, named after it.
fn project_dir_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".into())
}

fn branch_exists(root: &Path, branch: &str) -> bool {
    git(root)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .status()
        .is_ok_and(|status| status.success())
}

/// Git run in `dir`, without a terminal to ask anything in.
fn git(dir: &Path) -> Command {
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
    use std::fs;

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
            repo(&root),
            Some(Repo {
                root: root.clone(),
                prefix: PathBuf::new()
            })
        );
        assert_eq!(
            repo(&root.join("app")),
            Some(Repo {
                root: root.clone(),
                prefix: PathBuf::from("app")
            })
        );
        let outside = base.join("outside");
        fs::create_dir(&outside).unwrap();
        // The temp folder may itself be in a repository on some machines.
        if repo(&base).is_none() {
            assert_eq!(repo(&outside), None);
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
        .unwrap();
        assert_eq!(worktree.folder, worktree.root.join("app"));
        assert!(worktree.root.starts_with(base.join("worktrees/repo")));
        assert!(worktree.folder.join("main.rs").exists());
        let branch = branch_of(&worktree.root);
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
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default()).unwrap();
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
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default()).unwrap();
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
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default()).unwrap();
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
        .unwrap();
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
        let worktree = create_in(&root, &base.join("worktrees"), &Mutex::default()).unwrap();
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

    #[test]
    fn abandoning_keeps_a_checkout_its_chat_took() {
        let (base, root) = setup();
        let checkout = Checkout::start_in(&root, Some(base.join("worktrees")));
        let worktree = checkout.created().recv_blocking().unwrap().unwrap();
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
