//! Quitting cleanly on SIGTERM, as at logout or from `systemctl`.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::FromRawFd as _;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;

use gpui_kit::*;

/// Where the SIGTERM handler says that it ran.
static SIGTERM_PIPE: AtomicI32 = AtomicI32::new(-1);

/// Quits as the tray's Quit does on SIGTERM, so that every process started in
/// Agents Hub is stopped and what is queued is saved, instead of the app dying
/// with them running.
pub fn quit_on_sigterm(cx: &mut App) {
    extern "C" fn on_sigterm(_: libc::c_int) {
        let byte = 1u8;
        // SAFETY: write is async-signal-safe, and `byte` outlives the call.
        unsafe {
            libc::write(
                SIGTERM_PIPE.load(Ordering::Relaxed),
                std::ptr::from_ref(&byte).cast(),
                1,
            )
        };
    }

    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        eprintln!("can't watch for SIGTERM: {}", io::Error::last_os_error());
        return;
    }
    let [read, write] = fds;
    // A handler that blocked on a full pipe would hang whatever thread it
    // interrupted.
    // SAFETY: fcntl takes no pointers.
    unsafe { libc::fcntl(write, libc::F_SETFL, libc::O_NONBLOCK) };
    SIGTERM_PIPE.store(write, Ordering::Relaxed);
    // SAFETY: sigaction is plain data, for which all zeroes is a valid value.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: `action` is valid for the call, and its handler only writes to
    // the pipe, which stays open.
    if unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) } != 0 {
        eprintln!("can't watch for SIGTERM: {}", io::Error::last_os_error());
        return;
    }
    // SAFETY: `read` is the pipe's read end, which nothing else owns.
    let mut pipe = unsafe { File::from_raw_fd(read) };
    let (quit, asked) = smol::channel::bounded(1);
    thread::spawn(move || {
        if pipe.read_exact(&mut [0]).is_ok() {
            quit.try_send(()).ok();
        }
    });
    cx.spawn(async move |cx| {
        if asked.recv().await.is_ok() {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}
