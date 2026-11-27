//! Where a CLI's systemd scope comes from.
//!
//! The terminal model runs in the UI crate, which can't call the platform's
//! systemd adapter. The binary installs the adapter's functions once at
//! startup; tests run without one and skip the scope-specific cases.

use std::os::fd::OwnedFd;
use std::sync::OnceLock;
use std::sync::mpsc;

/// The platform's scope functions.
#[derive(Clone, Copy)]
pub struct Scope {
    /// A descriptor for `pid`, kept open until the CLI is reaped.
    pub pidfd: fn(u32) -> Option<OwnedFd>,
    /// Moves the process into a scope, returning the sender that stops it.
    pub watch: fn(String, Option<OwnedFd>, smol::channel::Sender<()>) -> mpsc::Sender<()>,
    /// Whether systemd can put a process in a scope at all.
    pub available: fn() -> bool,
    /// Stops every scope this app is waiting on.
    pub stop_all: fn(),
    /// Waits for every scope to be stopped and empty.
    pub wait_stopped: fn(),
    /// Moves an app started for the user into a scope that is never stopped.
    pub adopt: fn(u32, &str),
}

static SCOPE: OnceLock<Scope> = OnceLock::new();

/// Installs the platform's scope functions. Only the first call counts.
pub fn set(scope: Scope) {
    let _installed = SCOPE.set(scope);
}

pub(super) fn hooks() -> Scope {
    *SCOPE.get().unwrap_or(&DEFAULT)
}

fn no_pidfd(_: u32) -> Option<OwnedFd> {
    None
}

fn no_watch(_: String, _: Option<OwnedFd>, _: smol::channel::Sender<()>) -> mpsc::Sender<()> {
    let (stop, _) = mpsc::channel();
    stop
}

fn no_available() -> bool {
    false
}

fn no_stop() {}

fn no_wait() {}

fn no_adopt(_: u32, _: &str) {}

const DEFAULT: Scope = Scope {
    pidfd: no_pidfd,
    watch: no_watch,
    available: no_available,
    stop_all: no_stop,
    wait_stopped: no_wait,
    adopt: no_adopt,
};

pub(super) fn pidfd(pid: u32) -> Option<OwnedFd> {
    (hooks().pidfd)(pid)
}

pub(super) fn watch(
    program: String,
    pidfd: Option<OwnedFd>,
    gone: smol::channel::Sender<()>,
) -> mpsc::Sender<()> {
    (hooks().watch)(program, pidfd, gone)
}

pub(crate) fn adopt(pid: u32, desktop_id: &str) {
    (hooks().adopt)(pid, desktop_id);
}

#[cfg(test)]
pub(super) fn available() -> bool {
    (hooks().available)()
}

#[cfg(test)]
pub(super) fn stop_all() {
    (hooks().stop_all)();
}

#[cfg(test)]
pub(super) fn wait_stopped() {
    (hooks().wait_stopped)();
}
