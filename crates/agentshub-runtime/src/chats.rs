//! The chats' CLIs and their terminals: starting, stopping and restarting
//! them, what they report doing, and telling the user about it.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;

use agentshub_core::worktree::{self, Worktree};

/// Takes the worktree out of `job` and removes it, unless it was taken.
#[doc(hidden)]
pub fn take_and_remove(
    job: &Mutex<Option<(Worktree, PathBuf)>>,
) -> Option<Result<worktree::Removal>> {
    agentshub_core::lifecycle::take_and_remove(job, worktree::remove)
}
