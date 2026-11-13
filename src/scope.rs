//! Puts each CLI in a systemd scope of its own. A scope is a cgroup, which
//! every process the CLI starts stays in, even one that leaves its session as
//! the commands Claude Code runs do. Stopping the scope stops all of them, and
//! nothing that was never in it. Without a systemd user manager, stopping the
//! CLI's session is all there is.

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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

/// How long a stopped scope gets to be empty: what is left in it gets
/// `STOP_TIMEOUT` to exit, and systemd one answer's time to say it has.
const GONE_TIMEOUT: Duration = STOP_TIMEOUT.saturating_add(CALL_TIMEOUT);

/// How often a scope being stopped is checked for being empty.
const GONE_POLL: Duration = Duration::from_millis(20);

/// How long quitting waits for scopes to be stopped: a scope may still be
/// getting started when quitting stops it.
const QUIT_TIMEOUT: Duration = GONE_TIMEOUT.saturating_add(CALL_TIMEOUT);

/// Tests name their scopes apart from the app's, so that neither can take the
/// other's for leftovers to stop.
const PREFIX: &str = if cfg!(test) {
    "agentshub-test"
} else {
    "agentshub"
};

/// How many scopes are left to stop and empty.
static STOPPING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// What each scope still waiting to be stopped waits on, by its number.
static WAITING: Mutex<Vec<(u64, mpsc::Sender<()>)>> = Mutex::new(Vec::new());

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
struct Stopping(());

fn stopping() -> Stopping {
    *STOPPING.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    Stopping(())
}

impl Drop for Stopping {
    fn drop(&mut self) {
        *STOPPING.0.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        STOPPING.1.notify_all();
    }
}

/// Has every scope stopped, as quitting does, including those of CLIs that
/// couldn't be reaped in time, such as one stuck on a network filesystem.
pub fn stop_all() {
    for (_, stop) in WAITING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
    {
        stop.send(()).ok();
    }
}

/// Waits for every scope to be stopped and empty, for at most `QUIT_TIMEOUT`.
/// systemd goes on stopping them once asked, whether or not Agents Hub is
/// still running.
pub fn wait_stopped() {
    let (count, stopped) = &STOPPING;
    let count = count.lock().unwrap_or_else(PoisonError::into_inner);
    stopped
        .wait_timeout_while(count, QUIT_TIMEOUT, |count| *count > 0)
        .ok();
}

/// A pidfd for `pid`, which must not have been reaped yet. It goes on
/// referring to that process once the pid belongs to another.
pub fn pidfd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open takes no pointers.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0 as libc::c_uint) };
    // SAFETY: a descriptor pidfd_open returns is new, and nothing else owns it.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

/// Moves the process `pidfd` refers to, a CLI that `program` started, into a
/// scope of its own, on a thread of its own so that a slow systemd never holds
/// the CLI up. The scope is stopped once the sender this returns sends, as the
/// CLI's reaper does, or `stop_all` is called. `gone` is dropped once the
/// scope is empty, and quitting waits for that from now on.
pub fn watch(
    program: String,
    pidfd: Option<OwnedFd>,
    gone: impl Send + 'static,
) -> mpsc::Sender<()> {
    let stopping = stopping();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (stop_tx, stop) = mpsc::channel();
    WAITING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((id, stop_tx.clone()));
    thread::spawn(move || {
        let _stopping = stopping;
        let _gone = gone;
        let scope = pidfd.and_then(|pidfd| start(&program, id, pidfd));
        stop.recv().ok();
        WAITING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(waiting, _)| *waiting != id);
        if let Some(scope) = scope {
            scope.stop();
        }
    });
    stop_tx
}

/// Moves the process `pidfd` refers to into a new scope.
fn start(program: &str, id: u64, pidfd: OwnedFd) -> Option<Scope> {
    let bus = BUS.as_ref()?;
    let scope = Scope {
        name: format!("{PREFIX}-{}-{id}.scope", *RUN),
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
    /// Has systemd stop what is in the scope, SIGTERM, then SIGKILL once
    /// `STOP_TIMEOUT` is over, and waits for it to be empty.
    fn stop(self) {
        if stop(&self.name) {
            wait_gone(&self.name);
        }
    }
}

/// Asks systemd to stop a scope, and says whether it may still be stopping.
fn stop(name: &str) -> bool {
    let Some(bus) = BUS.as_ref() else {
        return false;
    };
    match bus.call_method(
        Some(DESTINATION),
        PATH,
        Some(MANAGER),
        "StopUnit",
        &(name, "replace"),
    ) {
        Ok(_) => true,
        // A scope goes by itself once nothing is left in it.
        Err(zbus::Error::MethodError(error, ..))
            if error.as_str() == "org.freedesktop.systemd1.NoSuchUnit" =>
        {
            false
        }
        Err(err) => {
            eprintln!("can't stop {name}: {err}");
            false
        }
    }
}

/// Waits for a scope being stopped to be empty, for at most `GONE_TIMEOUT`.
fn wait_gone(name: &str) {
    let Some(bus) = BUS.as_ref() else {
        return;
    };
    let deadline = Instant::now() + GONE_TIMEOUT;
    while Instant::now() < deadline {
        let units = bus
            .call_method(
                Some(DESTINATION),
                PATH,
                Some(MANAGER),
                "ListUnitsByNames",
                &(vec![name],),
            )
            .and_then(|reply| reply.body().deserialize::<Vec<Unit>>());
        match units {
            // One that is gone is listed as inactive, and not found.
            Ok(units)
                if units
                    .iter()
                    .all(|(_, _, _, active, ..)| active == "inactive" || active == "failed") =>
            {
                return;
            }
            Ok(_) => thread::sleep(GONE_POLL),
            Err(err) => {
                eprintln!("can't tell whether {name} has stopped: {err}");
                return;
            }
        }
    }
    eprintln!("{name} is still stopping");
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
            &(Vec::<&str>::new(), vec![format!("{PREFIX}-*.scope")]),
        )
        .and_then(|reply| reply.body().deserialize::<Vec<Unit>>());
    let units = match units {
        Ok(units) => units,
        Err(err) => {
            eprintln!("can't look for scopes left by a previous run: {err}");
            return;
        }
    };
    let ours = format!("{PREFIX}-{}-", *RUN);
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
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let scope = pidfd(child.id()).and_then(|pidfd| start("sleep", id, pidfd));
    let available = scope.is_some();
    if let Some(scope) = scope {
        scope.stop();
    }
    child.kill().ok();
    child.wait().ok();
    available
}
