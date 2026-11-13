//! Puts each CLI in a systemd scope of its own. A scope is a cgroup, which
//! every process the CLI starts stays in, even one that leaves its session as
//! the commands Claude Code runs do. Stopping the scope stops all of them, and
//! nothing that was never in it. Without a systemd user manager, stopping the
//! CLI's session is all there is.

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{Fd, OwnedObjectPath, Value};

const DESTINATION: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";

/// How long what is left in a scope gets to handle SIGTERM before systemd
/// kills it, which it does even after Agents Hub has quit.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// How long systemd gets to answer, so a stuck one can't hold up quitting.
const CALL_TIMEOUT: Duration = Duration::from_millis(500);

/// How long quitting waits for scopes to be stopped: a scope may still be
/// getting started when its CLI is gone, and then it gets stopped.
const QUIT_TIMEOUT: Duration = CALL_TIMEOUT.saturating_mul(2);

/// How many scopes are left to stop of CLIs that are gone.
static STOPPING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

static BUS: LazyLock<Option<Connection>> = LazyLock::new(|| {
    zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.method_timeout(CALL_TIMEOUT).build())
        .ok()
});

/// Scopes are named after the run that started them, which tells the ones a
/// crashed run left apart from this run's.
static RUN: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());
static NEXT: AtomicU64 = AtomicU64::new(0);

/// An entry of systemd's `ListUnitsByPatterns`, which starts with the name.
type Unit = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

pub struct Scope {
    name: String,
}

/// Counts a scope as left to stop until dropped.
pub struct Stopping(());

pub fn stopping() -> Stopping {
    *STOPPING.0.lock().unwrap() += 1;
    Stopping(())
}

impl Drop for Stopping {
    fn drop(&mut self) {
        *STOPPING.0.lock().unwrap() -= 1;
        STOPPING.1.notify_all();
    }
}

/// Waits for the scopes of CLIs that are gone to be stopped, for at most
/// `QUIT_TIMEOUT`. systemd goes on stopping them once asked, whether or not
/// Agents Hub is still running.
pub fn wait_stopped() {
    let (count, stopped) = &STOPPING;
    let _ = stopped.wait_timeout_while(count.lock().unwrap(), QUIT_TIMEOUT, |count| *count > 0);
}

/// A pidfd for `pid`, which must not have been reaped yet. It goes on
/// referring to that process once the pid belongs to another.
pub fn pidfd(pid: u32) -> Option<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0 as libc::c_uint) };
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

/// Moves the process `pidfd` refers to into a new scope.
pub fn start(program: &str, pidfd: OwnedFd) -> Option<Scope> {
    let bus = BUS.as_ref()?;
    let scope = Scope {
        name: format!(
            "agentshub-{}-{}.scope",
            *RUN,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let properties = vec![
        (
            "Description",
            Value::from(format!("{program} in Agents Hub")),
        ),
        ("PIDFDs", Value::from(vec![Fd::from(pidfd)])),
        ("CollectMode", Value::from("inactive-or-failed")),
        (
            "TimeoutStopUSec",
            Value::from(STOP_TIMEOUT.as_micros() as u64),
        ),
    ];
    let aux: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    match bus.call_method(
        Some(DESTINATION),
        PATH,
        Some(MANAGER),
        "StartTransientUnit",
        &(scope.name.as_str(), "fail", properties, aux),
    ) {
        Ok(_) => Some(scope),
        Err(err @ zbus::Error::MethodError(..)) => {
            eprintln!("can't put {program} in a scope of its own: {err}");
            None
        }
        // Without an answer, systemd may have made the scope all the same.
        Err(err) => {
            eprintln!("can't tell whether {program} is in a scope of its own: {err}");
            Some(scope)
        }
    }
}

impl Scope {
    /// Has systemd stop what is in the scope: SIGTERM, then SIGKILL once
    /// `STOP_TIMEOUT` is over.
    pub fn stop(self) {
        stop(&self.name);
    }
}

fn stop(name: &str) {
    let Some(bus) = BUS.as_ref() else {
        return;
    };
    match bus.call_method(
        Some(DESTINATION),
        PATH,
        Some(MANAGER),
        "StopUnit",
        &(name, "replace"),
    ) {
        Ok(_) => {}
        // A scope goes by itself once nothing is left in it.
        Err(zbus::Error::MethodError(error, ..))
            if error.as_str() == "org.freedesktop.systemd1.NoSuchUnit" => {}
        Err(err) => eprintln!("can't stop {name}: {err}"),
    }
}

/// Stops the scopes a crashed run left, with what its CLIs had started. Only
/// the instance that owns its name on the session bus may call this, as the
/// scopes of any other instance there would be stopped too.
pub fn stop_leftovers() {
    let Some(bus) = BUS.as_ref() else {
        return;
    };
    let units = bus
        .call_method(
            Some(DESTINATION),
            PATH,
            Some(MANAGER),
            "ListUnitsByPatterns",
            &(Vec::<&str>::new(), vec!["agentshub-*.scope"]),
        )
        .and_then(|reply| reply.body().deserialize::<Vec<Unit>>());
    let units = match units {
        Ok(units) => units,
        Err(err) => {
            eprintln!("can't look for scopes left by a previous run: {err}");
            return;
        }
    };
    let ours = format!("agentshub-{}-", *RUN);
    for (name, ..) in units {
        if !name.starts_with(&ours) {
            stop(&name);
        }
    }
}

/// Whether systemd can put a process in a scope from its pidfd, which takes
/// systemd 255 or later.
#[cfg(test)]
pub fn available() -> bool {
    let Ok(mut child) = std::process::Command::new("sleep").arg("10").spawn() else {
        return false;
    };
    let scope = pidfd(child.id()).and_then(|pidfd| start("sleep", pidfd));
    let available = scope.is_some();
    if let Some(scope) = scope {
        scope.stop();
    }
    child.kill().ok();
    child.wait().ok();
    available
}
