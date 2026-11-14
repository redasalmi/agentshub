//! Finding the sessions Codex CLIs are in, as Codex doesn't report them:
//! whenever its session files change, among the files each keeps open.

use std::time::Duration;

use gpui_kit::*;
use notify::{RecursiveMode, Watcher as _};

use crate::agent::Agent;
use crate::sessions;

use super::Workspace;

/// Codex writes its session files in bursts, so they're looked at once it
/// pauses.
const CODEX_DELAY: Duration = Duration::from_millis(500);

/// Watches Codex's session files, and looks for the sessions of the Codex
/// CLIs whenever they change.
pub(super) fn watch_sessions(
    cx: &mut Context<Workspace>,
) -> (Option<notify::RecommendedWatcher>, Task<()>) {
    let (codex_tx, codex_changed) = smol::channel::unbounded();
    let codex_watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        // Look at the files only when they are written. Opening a rollout
        // to find its session is an access, and scanning on that would
        // schedule the next scan forever.
        if !matches!(
            event.kind,
            notify::EventKind::Create(_) | notify::EventKind::Modify(_)
        ) {
            return;
        }
        codex_tx.try_send(()).ok();
    })
    .inspect_err(|err| eprintln!("can't watch Codex's sessions: {err}"))
    .ok();
    let codex_events = cx.spawn(async move |this, cx| {
        while codex_changed.recv().await.is_ok() {
            cx.background_executor().timer(CODEX_DELAY).await;
            while codex_changed.try_recv().is_ok() {}
            let found = this.update(cx, |this, cx| {
                this.update_codex_watch();
                this.find_codex_sessions(cx);
            });
            if found.is_err() {
                break;
            }
        }
    });
    (codex_watcher, codex_events)
}

impl Workspace {
    /// Codex writes its session files as it goes, so the sessions of its
    /// terminals are looked for whenever those change.
    pub(super) fn find_codex_sessions(&mut self, cx: &mut Context<Self>) {
        // Each chat's report name identifies this terminal, not just its row:
        // A chat can restart its terminal while the lookup runs.
        //
        // A lookup can take longer than the watcher's delay, so a newer one
        // can finish first. Marking the chats a lookup looks at with its
        // generation lets a result that has been superseded be dropped rather
        // than move a chat back to the session it has already left.
        let mut chats = Vec::new();
        let looked_at = self.chats.iter_mut().filter_map(|chat| {
            if chat.agent != Agent::Codex {
                return None;
            }
            let pid = chat.terminal.as_ref()?.terminal.read(cx).pid();
            if !pid.may_be_running() {
                return None;
            }
            chats.push((chat.id, chat.report.clone(), pid));
            Some(&mut chat.codex)
        });
        let Some(lookup) = self.lifecycle.look_up_codex(looked_at) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            // Looking through a process's open files, and reading the first
            // line of the newest one, doesn't belong on the window's thread.
            let found = smol::unblock(move || {
                chats
                    .into_iter()
                    .filter_map(|(id, report, pid)| {
                        let (agent_id, path) = sessions::codex_session(&pid)?;
                        Some((id, report, path, agent_id))
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            let Ok(closing) = this.update(cx, |this, cx| {
                for (id, report, path, agent_id) in found {
                    // A chat with this id now is another chat, or the same
                    // one with a newer terminal: either way the result is
                    // about a process that has gone. The same goes for a
                    // lookup that a newer one has already replaced.
                    let current = this
                        .chats
                        .iter()
                        .find(|chat| chat.id == id && chat.report == report)
                        .is_some_and(|chat| {
                            chat.codex.looked_up_by(lookup)
                                && chat.transcript.as_ref() != Some(&path)
                        });
                    if !current {
                        continue;
                    }
                    this.record_sessions(id, vec![(agent_id, path, None)], cx);
                }
                let mut closing = Vec::new();
                for chat in &mut this.chats {
                    if chat.codex.looked_up(lookup) {
                        closing.push(chat.id);
                    }
                }
                closing
            }) else {
                return;
            };
            if closing.is_empty() {
                return;
            }
            // One closed while its CLI was exiting may not be reaped yet, so
            // its session is read once it is, and nothing else runs in that
            // session until then. The shown chat takes the keyboard with it,
            // which only its window can give back.
            let Ok(window) = this.read_with(cx, |this, _| this.window) else {
                return;
            };
            let closed = window.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| this.close_chats(closing.clone(), window, cx))
            });
            // Closed to the tray, the window has no keyboard to give back, and
            // opening it again focuses the shown chat.
            if !matches!(closed, Ok(Ok(()))) {
                this.update(cx, |this, cx| {
                    for id in closing {
                        this.suspend(id, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn update_codex_watch(&mut self) {
        let (Some(watcher), Some(dir)) = (&mut self.codex_watcher, sessions::codex_sessions_dir())
        else {
            return;
        };
        let wanted = if dir.is_dir() {
            Some((dir, RecursiveMode::Recursive))
        } else {
            dir.ancestors()
                .skip(1)
                .find(|dir| dir.is_dir())
                .map(|dir| (dir.to_path_buf(), RecursiveMode::NonRecursive))
        };
        if wanted == self.codex_watch {
            return;
        }
        if let Some((path, _)) = self.codex_watch.take() {
            watcher.unwatch(&path).ok();
        }
        if let Some((path, mode)) = wanted {
            match watcher.watch(&path, mode) {
                Ok(()) => self.codex_watch = Some((path, mode)),
                Err(err) => eprintln!("can't watch {}: {err}", path.display()),
            }
        }
    }
}
