//! Shared-state types for multi-instance synchronization.
//!
//! Multiple thurbox processes (the TUI, `thurbox-cli`, an automation tick) share
//! one SQLite database in WAL mode; each notices the others' commits by polling
//! `PRAGMA data_version` (`storage::Database::data_version`) and re-reading the
//! rows it cares about. What lives here is the data those readers exchange —
//! [`SharedSession`] and friends — not a sync engine: v1's snapshot/delta
//! machinery (`SyncState`, `StateDelta`) was retired when `SnapshotStore` took
//! over the `data_version` gate and the row reads.

pub mod state;

pub use state::{current_time_millis, SharedSession, SharedState, SharedWorktree};
