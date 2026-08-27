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
pub mod automations;
pub mod keybindings;
pub mod messages;
pub mod repo_bookmarks;
pub mod review;
mod schema;
pub use schema::SCHEMA_VERSION;
mod sessions;
mod settings;
pub use sessions::{DeletedSessionInfo, HookRow};
pub mod sync;
pub mod tasks;
mod worktrees;

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use uuid::Uuid;

use crate::session::AutomationAction;

/// Serialize a `Spawn` action's extra-repo list for the `action_extra_repos`
/// column. An empty list (the single-repo common case) stores `NULL`, so old
/// and new single-repo rows are byte-identical. Shared by the `tasks` and
/// `automations` storage layers.
pub(super) fn extra_repos_to_json(extra_repos: &[crate::session::ExtraRepo]) -> Option<String> {
    if extra_repos.is_empty() {
        return None;
    }
    // Serialization of a plain struct list cannot fail; fall back to `None`.
    serde_json::to_string(extra_repos).ok()
}

/// Decode the `action_extra_repos` column back into an extra-repo list.
/// `NULL`/empty/malformed → an empty list (a single-repo spawn), never an error.
pub(super) fn extra_repos_from_json(raw: Option<String>) -> Vec<crate::session::ExtraRepo> {
    raw.filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// The action-specific columns an [`AutomationAction`] is stored as, shared by
/// the `tasks` and `automations` tables (both carry an identical group). The
/// `action_kind` discriminant is stored separately (`AutomationAction::kind`).
pub(super) type ActionColumns = (
    Option<String>, // target_session
    Option<String>, // repo_path
    Option<String>, // worktree_branch
    Option<String>, // base_branch
    Option<String>, // agent
    Option<String>, // action_extra_repos (JSON)
    Option<String>, // action_command
);

/// Encode an action into its persisted columns (sans the `action_kind`
/// discriminant, which the caller derives via [`AutomationAction::kind`]).
pub(super) fn action_to_columns(action: &AutomationAction) -> ActionColumns {
    match action {
        AutomationAction::Send { session_id } => (
            Some(session_id.to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            extra_repos,
        } => (
            None,
            Some(repo_path.to_string_lossy().into_owned()),
            worktree_branch.clone(),
            base_branch.clone(),
            agent.clone(),
            extra_repos_to_json(extra_repos),
            None,
        ),
        AutomationAction::Exec { command } => {
            (None, None, None, None, None, None, Some(command.clone()))
        }
    }
}

/// Reconstruct an action from its `action_kind` discriminant + persisted
/// columns. An unrecognized `kind` decodes to `Spawn` (the automations
/// catch-all); callers that allow an action-less row (tasks) gate this on a
/// non-NULL `action_kind` themselves.
pub(super) fn action_from_columns(kind: &str, cols: ActionColumns) -> AutomationAction {
    let (target_session, repo_path, worktree_branch, base_branch, agent, extra_repos_json, command) =
        cols;
    match kind {
        "send" => AutomationAction::Send {
            session_id: target_session
                .unwrap_or_default()
                .parse()
                .unwrap_or_default(),
        },
        "exec" => AutomationAction::Exec {
            command: command.unwrap_or_default(),
        },
        _ => AutomationAction::Spawn {
            repo_path: PathBuf::from(repo_path.unwrap_or_default()),
            worktree_branch,
            base_branch,
            agent,
            extra_repos: extra_repos_from_json(extra_repos_json),
        },
    }
}

/// SQLite-backed database for application state.
pub struct Database {
    conn: Connection,
    /// Unique ID for this thurbox instance (used in audit trail).
    instance_id: String,
}

impl Database {
    /// Open or create a database at the given path. Runs schema migrations.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        schema::initialize(&conn)?;

        let db = Self {
            conn,
            instance_id: Uuid::new_v4().to_string(),
        };
        // Best-effort retention; opening the DB must not fail over old breadcrumbs.
        if let Err(e) = db.prune_audit_log() {
            tracing::warn!("Failed to prune audit log: {e}");
        }
        if let Err(e) = db.prune_old_messages() {
            tracing::warn!("Failed to prune session messages: {e}");
        }
        Ok(db)
    }

    /// Open a database whose schema is already in place, skipping the schema
    /// pass and the retention sweeps.
    ///
    /// For short-lived worker reads inside a process that already ran
    /// [`Self::open`] at startup: `initialize` replays ~35 `CREATE … IF NOT
    /// EXISTS` statements, sets `journal_mode = WAL` (which takes the write
    /// lock) and runs two `INSERT OR IGNORE` writes — a real cost when the
    /// creation flow's bookmark worker reopened every few seconds. Only the
    /// per-connection pragmas that affect *this* connection's behaviour are
    /// applied.
    pub fn open_existing(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        schema::apply_connection_pragmas(&conn)?;
        Ok(Self {
            conn,
            instance_id: Uuid::new_v4().to_string(),
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
