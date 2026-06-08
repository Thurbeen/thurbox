//! Worktree-to-main git sync state (Ctrl+S).
//!
//! Grouped out of the [`App`](super::App) god object. Fields are `pub(crate)`
//! so call-sites keep direct access (`self.worktree_sync.in_progress`). Kept
//! distinct from the inter-instance [`SyncState`](crate::sync::SyncState),
//! which polls for DB changes from other thurbox instances.

use std::sync::mpsc;

use crate::git;
use crate::session::SessionId;

/// State for the worktree-to-main git sync (`Ctrl+S`): a background thread
/// rebases each session's worktree onto `origin/main` and reports results over
/// a channel polled in `tick`.
#[derive(Default)]
pub(crate) struct WorktreeSyncState {
    /// Whether a sync run is currently in flight.
    pub(crate) in_progress: bool,
    /// Receives `(session_id, result)` from the background sync thread.
    pub(crate) rx: Option<mpsc::Receiver<(SessionId, git::SyncResult)>>,
    /// Number of sessions the in-flight run is syncing.
    pub(crate) pending: usize,
    /// Results collected so far for the in-flight run.
    pub(crate) completed: Vec<(SessionId, git::SyncResult)>,
}
