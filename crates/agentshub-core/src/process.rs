//! A CLI's process group: killing what it leaves behind, and keeping its pid
//! from being reused while another thread looks at it.

use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

/// How long a CLI gets to stop its session before what's left is killed.
pub const KILL_GRACE: Duration = Duration::from_millis(200);

/// How often it's checked whether the CLI has exited.
pub const KILL_POLL: Duration = Duration::from_millis(5);

/// Hangs up the CLI's session, then kills whatever is left of it once the
/// CLI has exited or its grace period is over, so processes it started, like
/// the native binary behind an npm launcher, a shell's jobs or anything that
/// ignores SIGHUP, don't outlive it. The CLI leads its own session, so the
/// session id is its pid. The CLI isn't reaped before the last signal, which
/// keeps that id from being reused by another session in the meantime.
#[cfg(unix)]
pub fn kill_session(pid: u32) {
    for group in session_groups(pid) {
        // SAFETY: killpg takes no pointers.
        unsafe { libc::killpg(group, libc::SIGHUP) };
    }
    let deadline = Instant::now() + KILL_GRACE;
    while !has_exited(pid) && Instant::now() < deadline {
        thread::sleep(KILL_POLL);
    }
    for group in session_groups(pid) {
        // SAFETY: killpg takes no pointers.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
}

/// The process groups in a session: the leader's, and on Linux, the ones
/// shells give their jobs.
#[cfg(unix)]
fn session_groups(session: u32) -> Vec<libc::pid_t> {
    let mut groups = vec![session as libc::pid_t];
    #[cfg(target_os = "linux")]
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // The command name can hold spaces and parentheses, so the fields
        // are counted from after its last `)`: state, ppid, pgrp, session.
        let Some((_, fields)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = fields.split_whitespace().skip(2);
        if let (Some(Ok(group)), Some(Ok(id))) = (
            fields.next().map(str::parse),
            fields.next().map(str::parse::<u32>),
        ) && id == session
        {
            groups.push(group);
        }
    }
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// Whether the child has exited, leaving it to be reaped.
#[cfg(unix)]
fn has_exited(pid: u32) -> bool {
    // SAFETY: siginfo_t is plain data, for which all zeroes is a valid value.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid siginfo_t that outlives the call.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // A child that is still running leaves `info` zeroed.
    // SAFETY: waitid with WEXITED fills in `info` for a child, which sets
    // si_pid, or leaves it zeroed.
    result != 0 || unsafe { info.si_pid() } != 0
}

/// A CLI's pid, which can be used on any thread.
#[derive(Clone)]
pub struct Pid(Arc<Mutex<Option<u32>>>);

impl Pid {
    pub fn from_shared(pid: Arc<Mutex<Option<u32>>>) -> Self {
        Self(pid)
    }

    /// Calls `f` with the pid, unless it has been reaped. The pid can't be
    /// reaped, and so can't belong to another process, until `f` returns.
    pub fn with<T>(&self, f: impl FnOnce(u32) -> Option<T>) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .and_then(f)
    }

    /// Whether the pid may not be reaped yet, without waiting for whoever
    /// holds it, which can be a lookup through the process's files.
    pub fn may_be_running(&self) -> bool {
        match self.0.try_lock() {
            Ok(pid) => pid.is_some(),
            Err(TryLockError::Poisoned(err)) => err.into_inner().is_some(),
            Err(TryLockError::WouldBlock) => true,
        }
    }

    /// Forgets the pid once the CLI is reaped.
    pub fn reaped(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}
