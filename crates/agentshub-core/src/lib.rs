//! Agents Hub's domain and backend.
//!
//! Nothing here knows about GPUI, windows or views: the database, transcripts,
//! sessions, reports, worktrees, process control and the chat lifecycle live
//! in this crate so the UI can't reach into them by accident.

pub mod agent;
pub mod lifecycle;
pub mod process;
pub mod reports;
#[cfg(target_os = "linux")]
pub mod scope;
pub mod sessions;
pub mod status;
pub mod store;
pub mod transcript;
pub mod worktree;
