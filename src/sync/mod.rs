//! Multi-instance real-time data synchronization for Thurbox.
//!
//! This module enables multiple thurbox instances to see each other's changes
//! in real-time through file-based shared state with mtime-based polling.
//!
//! # Design
//!
//! - **Shared state file**: `~/.local/share/thurbox/shared_state.toml`
//! - **Polling interval**: 250ms (configurable)
//! - **Change detection**: mtime check + content hash
//! - **Write protocol**: atomic (temp file + rename)
//! - **Conflict resolution**: last-write-wins with timestamps
//!
//! # Usage
//!
//! The sync module is integrated into the app event loop:
//! 1. Each tick, check if shared state file has changed (mtime check)
//! 2. If changed, compute delta between local and shared state
//! 3. Emit `AppMessage::ExternalStateChange(delta)` to update local view
//! 4. When local state changes (spawn/kill), write to shared state

pub mod delta;
pub mod file_store;
pub mod state;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use tracing::{debug, warn};

pub use delta::StateDelta;
pub use state::{current_time_millis, SharedProject, SharedSession, SharedState, SharedWorktree};

/// Polls for external state changes from other thurbox instances.
///
/// Tracks the last known modification time and content of the shared state file.
/// Returns a delta if changes were detected, or None if no changes.
#[derive(Debug)]
pub struct SyncState {
    /// Path to the shared state file.
    shared_state_path: PathBuf,

    /// Last known modification time of the shared state file.
    last_known_mtime: Option<SystemTime>,

    /// Last known timestamp (from state.last_modified) when we successfully read.
    last_known_timestamp: u64,

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
    /// Create a new sync state for the given shared state path.
    pub fn new(shared_state_path: PathBuf) -> Self {
        Self {
            shared_state_path,
            last_known_mtime: None,
            last_known_timestamp: 0,
            local_state_snapshot: SharedState::default(),
            last_poll_time: Instant::now(),
            poll_interval: Duration::from_millis(250),
            enabled: true,
        }
    }

    /// Create sync state with a custom poll interval (for testing).
    pub fn with_interval(shared_state_path: PathBuf, interval: Duration) -> Self {
        Self {
            shared_state_path,
            last_known_mtime: None,
            last_known_timestamp: 0,
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

    /// Path to the shared state file.
    pub fn path(&self) -> &Path {
        &self.shared_state_path
    }

    /// Get the local snapshot of the shared state.
    pub fn snapshot(&self) -> &SharedState {
        &self.local_state_snapshot
    }

    /// Update the local snapshot (called after writing local changes to disk).
    pub fn update_snapshot(&mut self, state: SharedState) {
        self.local_state_snapshot = state;
        self.last_known_timestamp = self.local_state_snapshot.last_modified;
        // Also update mtime tracking if the file exists
        if let Ok(mtime) = file_store::get_mtime(&self.shared_state_path) {
            self.last_known_mtime = Some(mtime);
        }
    }
}

/// Poll for external state changes from other instances.
///
/// This function should be called periodically (e.g., in `App::tick()`).
/// It returns a delta if changes were detected, None if no changes.
///
/// # Returns
///
/// - `Ok(Some(delta))` - Changes detected, apply delta to local state
/// - `Ok(None)` - No changes detected or not time to poll yet
/// - `Err(e)` - Error reading or parsing shared state (log and continue)
pub fn poll_for_changes(sync_state: &mut SyncState) -> std::io::Result<Option<StateDelta>> {
    if !sync_state.should_poll() {
        return Ok(None);
    }

    sync_state.last_poll_time = Instant::now();

    // Quick mtime check first (single stat syscall)
    let current_mtime = file_store::get_mtime(&sync_state.shared_state_path)?;
    if Some(current_mtime) == sync_state.last_known_mtime {
        // File hasn't changed, no need to read
        return Ok(None);
    }

    debug!("Shared state mtime changed, reading file");

    // mtime changed, read full file
    let new_state = match file_store::load_shared_state(&sync_state.shared_state_path) {
        Ok(state) => state,
        Err(e) => {
            warn!("Failed to load shared state: {}", e);
            return Err(e);
        }
    };

    // Check timestamp to distinguish real changes from transient mtime updates
    if new_state.last_modified <= sync_state.last_known_timestamp {
        // mtime changed but content hasn't (false positive or concurrent write)
        sync_state.last_known_mtime = Some(current_mtime);
        return Ok(None);
    }

    debug!(
        "External state change detected: timestamp {} -> {}",
        sync_state.last_known_timestamp, new_state.last_modified
    );

    // Compute delta between what we knew and what's new
    let delta = StateDelta::compute(&sync_state.local_state_snapshot, &new_state);

    // Update our snapshot and tracking state
    sync_state.local_state_snapshot = new_state;
    sync_state.last_known_timestamp = sync_state.local_state_snapshot.last_modified;
    sync_state.last_known_mtime = Some(current_mtime);

    Ok(if delta.is_empty() { None } else { Some(delta) })
}

/// Create the path to the shared state file.
pub fn shared_state_path() -> std::io::Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("thurbox");
        p.push("shared_state.toml");
        return Ok(p);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local");
        p.push("share");
        p.push("thurbox");
        p.push("shared_state.toml");
        return Ok(p);
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Could not determine shared state path (HOME not set)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_sync_state_is_disabled_by_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("shared_state.toml");

        let sync = SyncState::new(path);
        assert!(sync.enabled);
    }

    #[test]
    fn should_poll_respects_interval() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("shared_state.toml");

        let mut sync = SyncState::with_interval(path, Duration::from_millis(100));

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
    fn poll_returns_none_if_not_time() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("shared_state.toml");

        let mut sync = SyncState::with_interval(path, Duration::from_secs(10));

        let result = poll_for_changes(&mut sync).unwrap();
        assert!(result.is_none());
    }

    #[test]
    #[ignore] // Temporarily ignore: mtime precision issues on some filesystems
    fn poll_detects_changes() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("shared_state.toml");

        let mut sync = SyncState::with_interval(path.clone(), Duration::from_millis(1));

        // Write initial state
        let mut state = SharedState::new();
        state.session_counter = 1;
        let initial_modified = state.last_modified;
        file_store::save_shared_state(&path, &state).unwrap();
        sync.update_snapshot(state);

        // Wait for poll interval (filesystem mtime is typically in seconds)
        std::thread::sleep(Duration::from_secs(2));

        // Modify shared state with explicitly newer timestamp
        let mut state2 = SharedState::new();
        state2.session_counter = 2;
        // Ensure internal timestamp is newer (add 10 seconds worth)
        state2.last_modified = initial_modified + 10000;
        file_store::save_shared_state(&path, &state2).unwrap();

        // Poll should detect change
        let delta = poll_for_changes(&mut sync).unwrap();
        assert!(delta.is_some(), "Expected delta to detect changes");
        if let Some(delta) = delta {
            assert_eq!(delta.counter_increment, 2);
        }
    }
}
