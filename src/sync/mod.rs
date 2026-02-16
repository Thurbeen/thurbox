//! Multi-instance real-time data synchronization for Thurbox.
//!
//! This module enables multiple thurbox instances to see each other's changes
//! in real-time through SQLite-based shared state with `PRAGMA data_version` polling.
//!
//! # Design
//!
//! - **Shared state**: SQLite database at `~/.local/share/thurbox/thurbox.db`
//! - **Polling interval**: 250ms (configurable)
//! - **Change detection**: SQLite `PRAGMA data_version` (increments on external writes in WAL mode)
//! - **Write protocol**: SQLite WAL mode handles concurrency automatically
//! - **Conflict resolution**: SQLite serializes writes via WAL
//!
//! # Usage
//!
//! The sync module is integrated into the app event loop:
//! 1. Each tick, check if database has changed (`PRAGMA data_version`)
//! 2. If changed, compute delta between local and DB state
//! 3. Apply delta to update local view
//! 4. When local state changes (spawn/kill), write to database

pub mod delta;
pub mod state;

use std::time::{Duration, Instant};

use tracing::debug;

pub use delta::StateDelta;
pub use state::{current_time_millis, SharedProject, SharedSession, SharedState, SharedWorktree};

/// Tracks polling state for external change detection.
///
/// Uses a time-based polling interval to avoid checking the database
/// on every tick. The local state snapshot is used to compute deltas
/// when changes are detected.
#[derive(Debug)]
pub struct SyncState {
    /// Snapshot of the shared state as we know it.
    local_state_snapshot: SharedState,

    /// When we last polled for changes.
    last_poll_time: Instant,

    /// How often to poll for external changes.
    poll_interval: Duration,

    /// Whether syncing is enabled.
    enabled: bool,
}

impl SyncState {
    /// Create a new sync state with the default 250ms poll interval.
    pub fn new() -> Self {
        Self {
            local_state_snapshot: SharedState::default(),
            last_poll_time: Instant::now(),
            poll_interval: Duration::from_millis(250),
            enabled: true,
        }
    }

    /// Create sync state with a custom poll interval (for testing).
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            local_state_snapshot: SharedState::default(),
            last_poll_time: Instant::now(),
            poll_interval: interval,
            enabled: true,
        }
    }

    /// Disable sync (useful for single-instance deployments or testing).
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Check if it's time to poll based on the configured interval.
    fn should_poll(&self) -> bool {
        self.enabled && self.last_poll_time.elapsed() >= self.poll_interval
    }
}

impl Default for SyncState {
    fn default() -> Self {
        Self::new()
    }
}

/// Poll for external state changes using the SQLite database.
///
/// Uses `PRAGMA data_version` for change detection, which increments
/// whenever another connection modifies the database in WAL mode.
///
/// # Returns
///
/// - `Ok(Some(delta))` - Changes detected, apply delta to local state
/// - `Ok(None)` - No changes detected or not time to poll yet
/// - `Err(e)` - Error querying database
pub fn poll_for_changes(
    sync_state: &mut SyncState,
    db: &mut crate::storage::Database,
) -> std::io::Result<Option<StateDelta>> {
    if !sync_state.should_poll() {
        return Ok(None);
    }

    sync_state.last_poll_time = Instant::now();

    let changed = db
        .has_external_changes()
        .map_err(|e| std::io::Error::other(format!("DB check failed: {e}")))?;

    if !changed {
        return Ok(None);
    }

    debug!("External DB change detected via PRAGMA data_version");

    let new_state = db
        .load_shared_state()
        .map_err(|e| std::io::Error::other(format!("Failed to load DB state: {e}")))?;

    let delta = StateDelta::compute(&sync_state.local_state_snapshot, &new_state);

    sync_state.local_state_snapshot = new_state;

    Ok(if delta.is_empty() { None } else { Some(delta) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sync_state_is_enabled_by_default() {
        let sync = SyncState::new();
        assert!(sync.enabled);
    }

    #[test]
    fn should_poll_respects_interval() {
        let mut sync = SyncState::with_interval(Duration::from_millis(100));

        // First check should not poll (too soon)
        assert!(!sync.should_poll());

        // Wait for interval to pass
        std::thread::sleep(Duration::from_millis(110));

        // Now should poll
        assert!(sync.should_poll());

        // Update poll time
        sync.last_poll_time = Instant::now();

        // Should not poll again immediately
        assert!(!sync.should_poll());
    }

    #[test]
    fn disable_prevents_polling() {
        let mut sync = SyncState::new();
        sync.last_poll_time = Instant::now() - Duration::from_secs(10);
        assert!(sync.should_poll());

        sync.disable();
        assert!(!sync.should_poll());
    }
}
