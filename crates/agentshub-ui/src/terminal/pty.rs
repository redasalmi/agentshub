//! Starting a CLI in a PTY, reaping it and killing what it leaves behind.

use super::*;

/// A started CLI process, before it is attached to the UI.
pub struct Pty {
    pub(super) tx: mpsc::Sender<Msg>,
    pub(super) updates: smol::channel::Receiver<Update>,
    pub(super) pid: Arc<Mutex<Option<u32>>>,
    /// Asks for the CLI to be killed, without going through the terminal
    /// thread, which blocks while the CLI isn't reading its input.
    pub(super) stop: mpsc::Sender<()>,
    /// Closes once the CLI has been reaped, and nothing is left in its scope.
    pub(super) reaped: smol::channel::Receiver<()>,
}

impl Pty {
    pub fn spawn(
        program: &str,
        args: &[String],
        env: &[(&str, &OsStr)],
        cwd: &Path,
        colors: Colors,
    ) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize::default())?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (key, value) in env {
            command.env(key, value);
        }
        // Taken before the CLI starts, as nothing would stop it if they failed
        // once it has.
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair.slave.spawn_command(command)?;
        let pid = Arc::new(Mutex::new(child.process_id()));
        // Opened before the reaper starts, so it is the CLI's.
        #[cfg(target_os = "linux")]
        let pidfd = child.process_id().and_then(scope::pidfd);
        drop(pair.slave);

        let (tx, rx) = mpsc::channel();
        let (updates_tx, updates) = smol::channel::bounded(UPDATE_QUEUE);

        let output_tx = tx.clone();
        thread::spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            // Reads fail with EIO once every side of the PTY is closed.
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if output_tx.send(Msg::Output(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        });

        // Reaps the child on its own instead of waiting for the reader: a
        // descendant holding inherited PTY descriptors keeps the reader from
        // reaching EOF long after the CLI itself has exited. Killing happens
        // only here, so the pid, which may get reused once it is reaped, is
        // never signalled. The shared pid is cleared, under its lock, before
        // the child is reaped, so nobody else uses it after that either.
        // The terminal asks for the kill when it goes away, and so does its
        // thread when it ends.
        let (stop_tx, stop_rx) = mpsc::channel();
        let (reaped_tx, reaped) = smol::channel::bounded::<()>(1);
        let exit_tx = tx.clone();
        let reaper_pid = pid.clone();
        // The CLI's scope is stopped once the reaper is done, and the CLI only
        // counts as reaped once nothing is left in it: what it started can go
        // on writing in its folder until then. The CLI is only moved there
        // after it starts, so what it starts in the meantime stays out of the
        // scope.
        #[cfg(target_os = "linux")]
        let reaper_done = scope::watch(program.to_owned(), pidfd, reaped_tx.clone());
        thread::spawn(move || {
            let _reaped_tx = reaped_tx;
            let mut child = child;
            let success = loop {
                {
                    let mut pid = reaper_pid.lock().unwrap_or_else(PoisonError::into_inner);
                    match child.try_wait() {
                        Ok(None) => {}
                        status => {
                            *pid = None;
                            break matches!(status, Ok(Some(status)) if status.success());
                        }
                    }
                }
                match stop_rx.recv_timeout(EXIT_POLL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // Taken in a statement of its own, so the lock isn't
                        // held through the kill.
                        let pid = reaper_pid
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .take();
                        match pid {
                            #[cfg(unix)]
                            Some(pid) => kill_session(pid),
                            _ => {
                                child.kill().ok();
                            }
                        }
                        child.wait().ok();
                        break false;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            };
            exit_tx.send(Msg::Exited(success)).ok();
            #[cfg(target_os = "linux")]
            reaper_done.send(()).ok();
        });

        let failed_tx = updates_tx.clone();
        let run_stop_tx = stop_tx.clone();
        thread::spawn(move || {
            if let Err(err) = run(rx, pair.master, writer, colors, updates_tx) {
                eprintln!("terminal thread failed: {err:#}");
                // The CLI gets killed below and nothing reports it anymore.
                failed_tx.send_blocking(Update::Exited(false)).ok();
            }
            run_stop_tx.send(()).ok();
        });

        Ok(Self {
            tx,
            updates,
            pid,
            stop: stop_tx,
            reaped,
        })
    }
}

/// Waits for CLIs that were shut down to be reaped, for at most
/// `SHUTDOWN_TIMEOUT`.
pub fn wait_reaped(reaped: impl IntoIterator<Item = smol::channel::Receiver<()>>) {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    smol::block_on(async {
        for reaped in reaped {
            let timeout = async {
                smol::Timer::at(deadline).await;
                Ok(())
            };
            reaped.recv().or(timeout).await.ok();
        }
    });
}
