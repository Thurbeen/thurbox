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
pub mod session_events;
mod session_meta;
mod sessions;
mod settings;
pub use session_events::{EventReason, SessionEventKind, SessionEventRow};
pub use sessions::{DeletedSessionInfo, HookRow, SessionFacts};
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
        if let Err(e) = db.prune_session_events() {
            tracing::warn!("Failed to prune the session event log: {e}");
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

    /// Begin a transaction that takes the write lock **up front**.
    ///
    /// Every write here must start this way, and `BEGIN DEFERRED` — what
    /// `Connection::unchecked_transaction` gives — must not be used for one.
    /// A deferred transaction takes no lock, so one that reads before it writes
    /// upgrades mid-flight; and in WAL mode an upgrade whose read snapshot
    /// another connection has already overtaken fails with `SQLITE_BUSY`
    /// **without consulting `busy_timeout`** — immediately, however long
    /// [`schema::BUSY_TIMEOUT`] is. The database is shared by the TUI,
    /// `thurbox-cli` and every agent hook, so that is not a rare interleaving:
    /// it is what a restart recording its new pane hits while a hook reports a
    /// state change.
    ///
    /// Taking the lock at `BEGIN` costs nothing — these writes are short
    /// single-row updates — and it is the difference between waiting out a
    /// peer and failing in front of one. `reorder_sessions` has spelled the
    /// same rule out with a raw `BEGIN IMMEDIATE` since it was written.
    ///
    /// Unchecked because `Database` methods take `&self`; the connection is
    /// not shared across threads.
    pub(crate) fn write_transaction(&self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
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

    fn session_row(name: &str) -> crate::sync::SharedSession {
        crate::sync::SharedSession {
            id: crate::session::SessionId::default(),
            name: name.to_string(),
            agent: "claude".to_string(),
            backend_id: "%1".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    /// A session write must wait out a peer that commits underneath it.
    ///
    /// This is the interleaving a restart hits on a busy machine: another
    /// process is mid-write when the restart records the pane it just spawned.
    /// A `BEGIN DEFERRED` transaction that reads before it writes upgrades
    /// against a snapshot that peer has since superseded, and WAL answers that
    /// with `SQLITE_BUSY` **without consulting `busy_timeout`** — so it fails
    /// outright, however long the timeout is. `session_ops::restart` read that
    /// failure as "the row was deleted", killed the agent window it had just
    /// spawned, and left the session with no window at all.
    ///
    /// Deterministic rather than a load test, which matters for a defect that
    /// only shows under concurrency: the peer takes the lock **before** the
    /// write starts and commits only once the call is proven to be in flight,
    /// so there is no interleaving to get lucky about. Under WAL nothing but
    /// the write itself can block — reads and `BEGIN` do not — so a call that
    /// has not returned while the lock is held is a call whose snapshot is
    /// already older than the commit that follows.
    #[test]
    fn a_session_write_waits_out_a_peer_that_commits_underneath_it() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        let db = Database::open(&path).unwrap();
        let subject = session_row("restarting");
        let peer_row = session_row("peer");
        db.upsert_session(&subject).unwrap();
        db.upsert_session(&peer_row).unwrap();

        // The peer holds the write lock and has not committed yet.
        let peer = rusqlite::Connection::open(&path).unwrap();
        peer.busy_timeout(schema::BUSY_TIMEOUT).unwrap();
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let (started, call_started) = mpsc::channel();
        let (done, call_done) = mpsc::channel();
        let write_path = path.clone();
        let id = subject.id;
        let writer = std::thread::spawn(move || {
            let db = Database::open_existing(&write_path).unwrap();
            started.send(()).unwrap();
            let outcome = db.set_backend_id(id, "%42");
            let _ = done.send(());
            outcome
        });

        // In flight: the call has begun and, with the lock held, cannot have
        // finished. Both halves are checked — the second is what says the
        // snapshot below is genuinely the older one.
        call_started.recv().unwrap();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            assert!(
                call_done.try_recv().is_err(),
                "the write returned while the peer held the lock, so it never blocked"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        peer.execute(
            "UPDATE sessions SET name = 'renamed' WHERE id = ?1",
            rusqlite::params![peer_row.id.to_string()],
        )
        .unwrap();
        peer.execute_batch("COMMIT").unwrap();

        let stored = writer
            .join()
            .unwrap()
            .expect("a peer's commit must not fail this write");
        assert!(
            stored,
            "the row is still there, so the write must have hit it"
        );
        assert_eq!(
            db.get_session_by_id(subject.id)
                .unwrap()
                .unwrap()
                .backend_id,
            "%42",
            "the pane a restart spawned must be what the row ends up naming"
        );
    }

    /// The same rule under ordinary load rather than a staged interleaving.
    ///
    /// Weaker than the test above by design — it can only under-report, since a
    /// scheduler that never runs the peer between two of these writes simply
    /// finds nothing — so it is the load-shaped second opinion, not the gate.
    /// It is here because this is the shape the defect was found in: one peer
    /// writing session rows the way an agent hook does was enough to fail
    /// roughly one in eight of these.
    #[test]
    fn session_writes_survive_a_peer_writing_continuously() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        let db = Database::open(&path).unwrap();
        let subject = session_row("restarting");
        let peer_row = session_row("chatty");
        db.upsert_session(&subject).unwrap();
        db.upsert_session(&peer_row).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let peer_stop = Arc::clone(&stop);
        let peer_path = path.clone();
        let peer_id = peer_row.id;
        // What every agent hook does: `thurbox-cli session signal`, on its own
        // connection, as often as the agent changes state.
        let peer = std::thread::spawn(move || {
            let db = Database::open_existing(&peer_path).unwrap();
            let mut n = 0u64;
            let mut refused = 0u64;
            while !peer_stop.load(Ordering::Relaxed) {
                let state = if n % 2 == 0 { "working" } else { "done" };
                if db.set_hook_state(peer_id, state).is_err() {
                    refused += 1;
                }
                n += 1;
            }
            refused
        });

        let mut refused = Vec::new();
        for i in 0..500 {
            if let Err(e) = db.set_backend_id(subject.id, &format!("%{i}")) {
                refused.push(format!("write {i}: {e}"));
            }
        }
        stop.store(true, Ordering::Relaxed);
        let peer_refused = peer.join().unwrap();

        assert!(
            refused.is_empty(),
            "{} of 500 pane writes were refused under one peer; each one is a \
             restart killing the agent window it just spawned:\n{}",
            refused.len(),
            refused.join("\n")
        );
        assert_eq!(
            peer_refused, 0,
            "the peer's own status writes were refused {peer_refused} times; \
             a dropped hook report is a session whose status stops moving"
        );
    }
}
