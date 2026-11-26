//! Opening a chat and resuming its last session.

use super::*;

impl Workspace {
    /// Shows a chat, resuming its last session if its CLI doesn't run.
    /// Shows a chat, starting its CLI unless it runs. An archived chat, or
    /// one in an archived project, is brought back first.
    #[expect(
        clippy::too_many_lines,
        reason = "the states a chat can be in when opened, checked in order"
    )]
    pub fn open_chat(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.lifecycle.is_deleting(id) || self.is_removing(id, cx) {
            return;
        }
        if self.is_archived(id, cx) {
            self.change(Change::RestoreChat(id), cx);
        }
        // Reading a chat's last session takes long enough for another chat
        // to be asked for meanwhile. This is the request that a completion
        // compares itself with.
        let request = self.lifecycle.request(id);
        if self.chats.iter().any(|chat| chat.id == id) {
            self.activate(id, window, cx);
            return;
        }
        // A CLI that was stopped to free what it held opens again once it's
        // gone, so the two never run in one session.
        if let Some(reaped) = self.lifecycle.reaping(id) {
            cx.spawn_in(window, async move |this, cx| {
                reaped.recv().await.ok();
                this.update_in(cx, |this, window, cx| {
                    if this.lifecycle.is_requested(id) {
                        this.open_chat(id, window, cx);
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        // Clicks that arrive while the session is read would start a second
        // CLI in the same chat. A read that was cancelled starts it after all.
        if !self.lifecycle.open(id) {
            return;
        }

        // The last session decides how the CLI starts, so it's read first: the
        // CLI may have gone on without agentshub. Transcripts can be large
        // enough that this can't happen while the window waits.
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = async {
                let mut sessions = store.sessions(id).await?;
                let paths: Vec<PathBuf> = sessions
                    .iter()
                    .map(|session| session.transcript_path.clone())
                    .collect();
                let exist: Vec<bool> =
                    smol::unblock(move || paths.iter().map(|path| path.exists()).collect()).await;
                let mut exist = exist.into_iter();
                let last_exists = exist.next_back().unwrap_or(false);
                if let Some(last) = sessions.pop()
                    && last_exists
                {
                    store.read_transcript(last.id).await?;
                    // Opening a chat is the chat being active, which orders the
                    // sidebar. Reading its history must not do the same.
                    store.touch_session(last.id).await?;
                }
                // The history hides a session without a prompt, so it is never
                // read again there, even once a newer parser would find one.
                for (earlier, exists) in sessions.iter().zip(exist) {
                    if !earlier.prompted
                        && exists
                        && let Err(err) = store.read_transcript(earlier.id).await
                    {
                        eprintln!("can't read session {}: {err}", earlier.id);
                    }
                }
                anyhow::Ok(())
            }
            .await;
            if let Err(err) = read {
                eprintln!("can't read the last session of chat {id}: {err}");
            }
            // The chat as it is once read: it may have been deleted meanwhile,
            // and its last session found prompted.
            let opened = async {
                let chat = store.chat(id).await?;
                let sessions = store.sessions(id).await?;
                let path = sessions.last().map(|last| last.transcript_path.clone());
                let last_exists =
                    smol::unblock(move || path.is_some_and(|path| path.exists())).await;
                anyhow::Ok((chat, sessions, last_exists))
            }
            .await;
            if this
                .update_in(cx, |this, window, cx| {
                    // The chats may have been closed meanwhile.
                    if !this.lifecycle.opened(id) {
                        return;
                    }
                    // The chat may have been deleted, or opened elsewhere, meanwhile,
                    // or its project be being removed.
                    if this.chats.iter().any(|chat| chat.id == id) || this.is_removing(id, cx) {
                        return;
                    }
                    let (agent, cwd, sessions, last_exists) = match opened {
                        Ok((Some((agent, cwd)), sessions, last_exists)) => {
                            (agent, cwd, sessions, last_exists)
                        }
                        Ok((None, ..)) => {
                            // If the chat is gone, the one it took over the
                            // view from still shows when it opens.
                            if let Some(overtaken) = this.lifecycle.give_back(id, request)
                                && this.chats.iter().any(|chat| chat.id == overtaken)
                            {
                                this.activate(overtaken, window, cx);
                            }
                            return;
                        }
                        Err(err) => {
                            eprintln!("can't open chat {id}: {err}");
                            return;
                        }
                    };
                    let mut notice = None;
                    let resumed = match sessions.last() {
                        Some(last) if last.prompted && last_exists => Some(last),
                        // Claude deletes old transcripts. What the chat kept of the
                        // session stays in its history, and the banner says why a
                        // new session started.
                        Some(last) if last.prompted => {
                            let (session, lost) = (
                                last.id,
                                this.store.lose_transcript(last.id, TRANSCRIPT_GONE),
                            );
                            cx.background_spawn(async move {
                                if let Err(err) = lost.await {
                                    eprintln!("can't mark session {session} missing: {err}")
                                }
                            })
                            .detach();
                            notice = Some(TRANSCRIPT_GONE.into());
                            None
                        }
                        // A session without a prompt has nothing to resume.
                        _ => None,
                    };
                    // A chat the user selected while this one was being read
                    // keeps the view. This one still starts, but in the
                    // background, so its completion can't steal focus.
                    let show = this.lifecycle.is_requested(id);
                    // Two CLIs in one session would append to the same transcript,
                    // so a session another chat is running can't be resumed here.
                    if let Some(last) = resumed
                        && this.live_elsewhere(id, agent, last)
                    {
                        this.block_chat(
                            id,
                            agent,
                            &cwd,
                            sessions.last(),
                            "This session is running in another chat. Start a new session to \
                         keep going here."
                                .into(),
                            cx,
                        );
                    } else if this.start_chat(id, agent, &cwd, resumed, window, cx)
                        && let Some(notice) = notice
                        && let Some(chat) = this.chats.iter_mut().find(|chat| chat.id == id)
                    {
                        chat.notice = Some(notice);
                    }
                    if show {
                        this.activate(id, window, cx);
                    } else {
                        this.sync_sidebar(cx);
                        cx.notify();
                    }
                })
                .is_err()
            {
                // The window closed meanwhile, so the chat isn't started, but
                // it can be opened again.
                this.update(cx, |this, _| this.lifecycle.opened(id)).ok();
            }
        })
        .detach();
    }
}
