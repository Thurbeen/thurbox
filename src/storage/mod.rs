//! SQLite-backed persistent storage for Thurbox state.
//!
//! Replaces `state.toml` and `shared_state.toml` with a single SQLite database.
//! Provides soft delete with `deleted_at` columns and a full audit trail.
//!
//! # Usage
//!
//! ```ignore
//! let db = Database::open(path)?;
//! db.upsert_session(&session)?;
//! ```

pub mod audit;
pub mod containers;
pub mod keybindings;
mod mcp_servers;
mod plugin_settings;
mod plugins;
pub mod repo_bookmarks;
mod roles;
mod scheduled_commands;
mod schema;
mod sessions;
mod settings;
mod skills;
pub use mcp_servers::McpServerSource;
pub use plugin_settings::EffectiveSetting;
pub use plugins::PluginSource;
pub use roles::RoleSource;
pub use sessions::DeletedSessionInfo;
pub use skills::SkillSource;
pub mod sync;
pub mod vms;
mod worktrees;

use std::path::Path;

use rusqlite::Connection;
use uuid::Uuid;

/// SQLite-backed database for application state.
pub struct Database {
    conn: Connection,
    /// Unique ID for this thurbox instance (used in audit trail).
    instance_id: String,
    /// Last known data_version for external change detection.
    last_data_version: i64,
}

impl Database {
    /// Open or create a database at the given path. Runs schema migrations.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        schema::initialize(&conn)?;

        let last_data_version = conn.query_row("PRAGMA data_version", [], |row| row.get(0))?;

        Ok(Self {
            conn,
            instance_id: Uuid::new_v4().to_string(),
            last_data_version,
        })
    }

    /// Get a reference to the underlying connection (for metadata queries).
    pub fn conn_ref(&self) -> &Connection {
        &self.conn
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::initialize(&conn)?;

        Ok(Self {
            conn,
            instance_id: Uuid::new_v4().to_string(),
            last_data_version: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory() {
        let db = Database::open_in_memory();
        assert!(db.is_ok());
    }

    #[test]
    fn open_file_based() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let db = Database::open(temp.path());
        assert!(db.is_ok());
    }

    #[test]
    fn open_creates_parent_dirs() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("sub").join("dir").join("thurbox.db");

        let db = Database::open(&path);
        assert!(db.is_ok());
        assert!(path.exists());
    }

    #[test]
    fn instance_id_is_unique() {
        let db1 = Database::open_in_memory().unwrap();
        let db2 = Database::open_in_memory().unwrap();
        assert_ne!(db1.instance_id, db2.instance_id);
    }
}
