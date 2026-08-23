mod migrations;

use migrations::*;

use rusqlite::Connection;

/// Current schema version. Incremented when schema changes.
///
/// v29 is reserved by the in-flight `improve-agent-thurbox-cli` branch
/// (`session_labels` + `session_spawn_config`); v30 added
/// `parent_session_id`, v31 added `display_order`, v32 added
/// `session_messages` (the inter-session mailbox), v33 adds
/// `action_extra_repos` to `tasks` + `automations` (multi-repo spawns),
/// v34 adds `hook_state` / `hook_state_at` / `seen_at` to `sessions`
/// (hooks-driven session status); v35 adds `idx_tasks_external` on
/// `tasks(source, external_id)` (external-tracker sync lookup); v36 adds
/// `action_command` to `tasks` + `automations` (the `Exec` automation action);
/// v37 adds `force_deleted` to `sessions` (a hard delete tore down its
/// worktrees/tmux, so it can't be restored — the restore list tags + blocks it);
/// v38 adds `base_branch` to `sessions` plus the `review_comments` /
/// `review_marks` tables (the native code-review view); v39 scopes
/// `repo_bookmarks` to a `host` (`''` = local), giving remote targets the
/// same bookmark memory as local ones; v40 adds `is_git` (NULL = unknown,
/// gates the worktree toggle) and `parent_path` (persisted children of a
/// remote parent bookmark) to `repo_bookmarks`.
/// Gaps in the step table are fine (there is no v18 step either).
pub const SCHEMA_VERSION: u32 = 40;

/// A single migration step: applied when the stored version is below `target`.
type MigrationStep = (u32, fn(&Connection) -> rusqlite::Result<()>);

/// How long a connection waits on a locked database before erroring.
/// The DB is shared by the TUI, thurbox-cli, and the automation heartbeat;
/// writes are short single-row upserts, so 5 s outlasts any WAL checkpoint.
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Per-connection pragmas, without the schema pass.
///
/// Split out for [`crate::storage::Database::open_existing`]: everything here
/// affects only the connection it runs on (WAL is a database property once
/// set, so a second connection need not re-issue it — and re-issuing is what
/// took the write lock on every worker open).
pub fn apply_connection_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    // Performance pragmas (safe under WAL):
    // - `synchronous = NORMAL` is the WAL-recommended setting: a crash can't
    //   corrupt the DB, only lose the last few un-checkpointed commits — fine
    //   for this advisory session/automation state, and it avoids an fsync per
    //   commit.
    // - `cache_size = -8000` gives each connection an 8 MB page cache (negative
    //   = KiB), comfortably holding the small working set in memory.
    // - `mmap_size` memory-maps reads, skipping a copy through the page cache
    //   for the read-heavy polling workload.
    // - `temp_store = MEMORY` keeps transient indexes/sorts off disk.
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -8000;
         PRAGMA mmap_size = 67108864;
         PRAGMA temp_store = MEMORY;",
    )
}

/// Create all tables and indexes if they don't exist.
pub fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    // Must come first: the WAL pragma below itself needs the write lock.
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    apply_connection_pragmas(conn)?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id                TEXT PRIMARY KEY,
            name              TEXT NOT NULL,
            agent             TEXT NOT NULL DEFAULT 'claude',
            backend_id        TEXT NOT NULL DEFAULT '',
            backend_type      TEXT NOT NULL DEFAULT 'tmux',
            agent_session_id  TEXT,
            cwd               TEXT,
            additional_dirs   TEXT NOT NULL DEFAULT '',
            shell_backend_id  TEXT,
            parent_session_id TEXT,
            display_order     INTEGER,
            hook_state        TEXT,
            hook_state_at     INTEGER,
            seen_at           INTEGER,
            force_deleted     INTEGER NOT NULL DEFAULT 0,
            base_branch       TEXT,
            created_at        INTEGER NOT NULL,
            updated_at        INTEGER NOT NULL,
            deleted_at        INTEGER
        );

        CREATE TABLE IF NOT EXISTS review_comments (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id     TEXT NOT NULL,
            file_path      TEXT,
            side           TEXT,
            line_no        INTEGER,
            classification TEXT NOT NULL DEFAULT 'note',
            body           TEXT NOT NULL,
            created_at     INTEGER NOT NULL,
            updated_at     INTEGER NOT NULL,
            deleted_at     INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_review_comments_session
            ON review_comments(session_id) WHERE deleted_at IS NULL;

        CREATE TABLE IF NOT EXISTS review_marks (
            session_id  TEXT NOT NULL,
            file_path   TEXT NOT NULL,
            hunk_index  INTEGER NOT NULL DEFAULT -1,
            created_at  INTEGER NOT NULL,
            PRIMARY KEY (session_id, file_path, hunk_index)
        );

        CREATE TABLE IF NOT EXISTS worktrees (
            session_id    TEXT NOT NULL REFERENCES sessions(id),
            repo_path     TEXT NOT NULL,
            worktree_path TEXT NOT NULL,
            branch        TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            deleted_at    INTEGER,
            PRIMARY KEY (session_id, repo_path)
        );

        CREATE TABLE IF NOT EXISTS audit_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp   INTEGER NOT NULL,
            entity_type TEXT NOT NULL,
            entity_id   TEXT NOT NULL,
            action      TEXT NOT NULL,
            field       TEXT,
            old_value   TEXT,
            new_value   TEXT,
            instance_id TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_audit_log_entity
            ON audit_log(entity_type, entity_id);
        CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp
            ON audit_log(timestamp);
        CREATE INDEX IF NOT EXISTS idx_sessions_active
            ON sessions(id) WHERE deleted_at IS NULL;

        CREATE TABLE IF NOT EXISTS automations (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            name            TEXT NOT NULL,
            enabled         INTEGER NOT NULL DEFAULT 1,
            schedule_kind   TEXT NOT NULL,
            schedule_spec   TEXT NOT NULL,
            timezone        TEXT,
            action_kind     TEXT NOT NULL,
            target_session  TEXT,
            repo_path       TEXT,
            worktree_branch TEXT,
            base_branch     TEXT,
            agent           TEXT,
            prompt          TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL,
            last_run_at     INTEGER,
            next_run_at     INTEGER,
            action_extra_repos TEXT,
            action_command  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_automations_due
            ON automations(next_run_at)
            WHERE enabled = 1 AND next_run_at IS NOT NULL;

        CREATE TABLE IF NOT EXISTS automation_runs (
            id                 INTEGER PRIMARY KEY AUTOINCREMENT,
            automation_id      INTEGER NOT NULL,
            started_at         INTEGER NOT NULL,
            status             TEXT NOT NULL,
            detail             TEXT NOT NULL DEFAULT '',
            related_session_id TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_automation_runs_automation
            ON automation_runs(automation_id, started_at);

        CREATE TABLE IF NOT EXISTS repo_bookmarks (
            host         TEXT NOT NULL DEFAULT '',
            repo_path    TEXT NOT NULL,
            label        TEXT,
            last_used_at INTEGER NOT NULL,
            use_count    INTEGER NOT NULL DEFAULT 1,
            is_parent    INTEGER NOT NULL DEFAULT 0,
            is_git       INTEGER,
            parent_path  TEXT,
            PRIMARY KEY (host, repo_path)
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            title           TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'todo',
            action_kind     TEXT,
            target_session  TEXT,
            repo_path       TEXT,
            worktree_branch TEXT,
            base_branch     TEXT,
            agent           TEXT,
            source          TEXT NOT NULL DEFAULT 'local',
            external_id     TEXT,
            external_url    TEXT,
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL,
            deleted_at      INTEGER,
            description     TEXT,
            action_extra_repos TEXT,
            action_command  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_status
            ON tasks(status) WHERE deleted_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_tasks_external
            ON tasks(source, external_id) WHERE deleted_at IS NULL;

        CREATE TABLE IF NOT EXISTS session_messages (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            to_session_id   TEXT NOT NULL,
            from_session_id TEXT,
            from_task_id    INTEGER,
            kind            TEXT NOT NULL DEFAULT 'note',
            body            TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            read_at         INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_session_messages_unread
            ON session_messages(to_session_id) WHERE read_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_session_messages_created
            ON session_messages(created_at);
        ",
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO metadata (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION.to_string()],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO metadata (key, value) VALUES ('session_counter', '0')",
        [],
    )?;

    migrate(conn)?;

    Ok(())
}

/// Run schema migrations for existing databases.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: u32 = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| {
                let val: String = row.get(0)?;
                Ok(val.parse().unwrap_or(0))
            },
        )
        .unwrap_or(0);

    // Each migration step is gated on the stored version and applied in order.
    // Steps are extracted into helpers to keep this dispatcher flat.
    let steps: &[MigrationStep] = &[
        (3, migrate_v3_additional_dirs),
        (4, migrate_v4_project_mcp_servers),
        (5, migrate_v5_session_commands),
        (6, migrate_v6_worktrees_pk),
        (7, migrate_v7_shell_backend_id),
        (8, migrate_v8_vms),
        (9, migrate_v9_agent_session_id),
        (10, migrate_v10_containers),
        (11, migrate_v11_containerfile),
        (12, migrate_v12_scheduled_commands),
        (13, migrate_v13_roles),
        (14, migrate_v14_mcp_servers),
        (15, migrate_v15_nullable_project_id),
        (16, migrate_v16_drop_projects),
        (17, migrate_v17_skills),
        (19, migrate_v19_plugins),
        (20, migrate_v20_profiles),
        (21, migrate_v21_drop_model),
        (22, migrate_v22_drop_subsystems),
        (23, migrate_v23_generic_agent),
        (24, migrate_v24_automations),
        (25, migrate_v25_tasks),
        (26, migrate_v26_task_description),
        (27, migrate_v27_repo_parent_bookmarks),
        (28, migrate_v28_run_related_session),
        (30, migrate_v30_parent_session_id),
        (31, migrate_v31_display_order),
        (32, migrate_v32_session_messages),
        (33, migrate_v33_action_extra_repos),
        (34, migrate_v34_hook_status),
        (35, migrate_v35_tasks_external_index),
        (36, migrate_v36_action_command),
        (37, migrate_v37_force_deleted),
        (38, migrate_v38_code_review),
        (39, migrate_v39_bookmark_host),
        (40, migrate_v40_bookmark_git_kind),
    ];

    for &(target, step) in steps {
        if version < target {
            step(conn)?;
        }
    }

    if version < SCHEMA_VERSION {
        conn.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            [SCHEMA_VERSION.to_string()],
        )?;
    }

    Ok(())
}

// The `ALTER TABLE` migrations below were historically written as
// `let _ = conn.execute("ALTER …")` to stay idempotent on re-runs (the column is
// already present). But that swallowed *every* error — including genuine
// failures — while `SCHEMA_VERSION` still advanced, silently leaving an
// inconsistent schema marked as upgraded. The helpers here make the *benign*
// "already applied" case a true no-op (decided up front via a `PRAGMA` check
// rather than by matching SQLite's error text, which is locale/version
// dependent) and propagate any *real* failure via `?`, so a failed migration
// aborts `migrate` before the version bump and the stored version stays put.

/// Return whether a table named `table` exists.
pub(super) fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists([table])
}

/// Return whether `table` has a column named `column` (false if the table is
/// absent: `pragma_table_info` yields no rows for a missing table).
pub(super) fn column_exists(
    conn: &Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    // `table` is always a hardcoded identifier from the migration code below
    // (never user input), so interpolating it into the PRAGMA is injection-safe.
    let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1");
    conn.prepare(&sql)?.exists([column])
}

/// `ALTER TABLE <table> ADD COLUMN <column> <decl>`, but only when the table
/// exists and the column does not. The skip-when-present check keeps the
/// migration idempotent without swallowing errors; a genuine `ALTER` failure
/// propagates. A missing table is a no-op — in a real upgrade path the table is
/// always created by an earlier step, so this never hides a column we needed.
pub(super) fn add_column_if_absent(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> rusqlite::Result<()> {
    if !table_exists(conn, table)? || column_exists(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
        [],
    )?;
    Ok(())
}

/// `ALTER TABLE <table> DROP COLUMN <column>`, but only when both exist. Already
/// dropped (or table absent) is a no-op; a real `DROP` failure propagates.
pub(super) fn drop_column_if_present(
    conn: &Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<()> {
    if !table_exists(conn, table)? || !column_exists(conn, table, column)? {
        return Ok(());
    }
    conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])?;
    Ok(())
}

/// `ALTER TABLE <table> RENAME COLUMN <from> TO <to>`, but only when the source
/// column is present. Already renamed (or table absent) is a no-op; a real
/// `RENAME` failure propagates.
pub(super) fn rename_column_if_present(
    conn: &Connection,
    table: &str,
    from: &str,
    to: &str,
) -> rusqlite::Result<()> {
    if !table_exists(conn, table)? || !column_exists(conn, table, from)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} RENAME COLUMN {from} TO {to}"),
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_sets_busy_timeout() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let timeout_ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout_ms, BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn schema_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(tables.contains(&"metadata".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"worktrees".to_string()));
        assert!(tables.contains(&"audit_log".to_string()));
        assert!(tables.contains(&"automations".to_string()));
        assert!(tables.contains(&"automation_runs".to_string()));
        assert!(tables.contains(&"repo_bookmarks".to_string()));
        assert!(tables.contains(&"tasks".to_string()));
        assert!(tables.contains(&"session_messages".to_string()));
        // The legacy one-shot table is replaced by `automations`.
        assert!(!tables.contains(&"scheduled_commands".to_string()));
        // Dropped Claude-config tables should NOT exist.
        assert!(!tables.contains(&"roles".to_string()));
        assert!(!tables.contains(&"mcp_servers".to_string()));
        assert!(!tables.contains(&"skills".to_string()));
        assert!(!tables.contains(&"profiles".to_string()));
    }

    #[test]
    fn migration_v24_moves_pending_scheduled_commands() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal legacy (v23) state: metadata + a scheduled_commands table with
        // one pending and one already-executed row.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '23');
             CREATE TABLE scheduled_commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                command_text TEXT NOT NULL, scheduled_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL, executed_at INTEGER, cancelled_at INTEGER);
             INSERT INTO scheduled_commands
                (session_id, command_text, scheduled_at, created_at, executed_at, cancelled_at)
                VALUES ('s1', 'pending cmd', 5000, 100, NULL, NULL),
                       ('s1', 'done cmd', 6000, 100, 6000, NULL);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        // Legacy table is dropped.
        let has_legacy: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='scheduled_commands'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!has_legacy, "scheduled_commands should be dropped");

        // Only the pending row is carried forward, as a once/send automation.
        let count: i64 = conn
            .query_row("SELECT count(*) FROM automations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let (kind, action, prompt, next): (String, String, String, i64) = conn
            .query_row(
                "SELECT schedule_kind, action_kind, prompt, next_run_at FROM automations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "once");
        assert_eq!(action, "send");
        assert_eq!(prompt, "pending cmd");
        assert_eq!(next, 5000);
    }

    #[test]
    fn migrate_from_v24_adds_tasks_table() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v24 state: metadata pinned to 24, no tasks table.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '24');",
        )
        .unwrap();

        let has_tasks_before: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='tasks'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!has_tasks_before);

        migrate(&conn).unwrap();

        let has_tasks_after: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='tasks'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_tasks_after, "tasks table should be created at v25");

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v25_adds_description_column() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v25 state: a tasks table without the description column.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '25');
             CREATE TABLE tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'todo', action_kind TEXT,
                target_session TEXT, repo_path TEXT, worktree_branch TEXT,
                base_branch TEXT, agent TEXT, source TEXT NOT NULL DEFAULT 'local',
                external_id TEXT, external_url TEXT, created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL, deleted_at INTEGER);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let has_description: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('tasks') WHERE name='description'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_description, "description column should be added at v26");

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v36_adds_force_deleted_column() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v36 state: a sessions table without the force_deleted column.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '36');
             CREATE TABLE sessions (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                deleted_at INTEGER);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let has_col: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='force_deleted'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_col, "force_deleted column should be added at v37");

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v38_rebuilds_repo_bookmarks_with_host_key() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v38 state: the pre-v39 repo_bookmarks shape with one row.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '38');
             CREATE TABLE repo_bookmarks (
                repo_path    TEXT PRIMARY KEY,
                label        TEXT,
                last_used_at INTEGER NOT NULL,
                use_count    INTEGER NOT NULL DEFAULT 1,
                is_parent    INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO repo_bookmarks (repo_path, last_used_at, use_count, is_parent)
             VALUES ('/repo/a', 1, 3, 1);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        // The existing row migrated as local ('') with its fields intact.
        let (host, count, is_parent): (String, i64, i64) = conn
            .query_row(
                "SELECT host, use_count, is_parent FROM repo_bookmarks \
                 WHERE repo_path = '/repo/a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(host, "");
        assert_eq!(count, 3);
        assert_eq!(is_parent, 1);

        // The rebuilt (host, repo_path) key allows the same path on another
        // host — the whole point of the rebuild.
        conn.execute(
            "INSERT INTO repo_bookmarks (host, repo_path, last_used_at) \
             VALUES ('ssh:devbox', '/repo/a', 2)",
            [],
        )
        .unwrap();

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_v39_recovers_from_a_crashed_prior_run() {
        let conn = Connection::open_in_memory().unwrap();
        // v38 state plus the orphan `repo_bookmarks_v39` a pre-fix binary could
        // strand by crashing between its CREATE and the version bump. The
        // re-run must repair it (DROP prefix), not fail with "table
        // repo_bookmarks_v39 already exists" — which made the DB unopenable.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '38');
             CREATE TABLE repo_bookmarks (
                repo_path    TEXT PRIMARY KEY,
                label        TEXT,
                last_used_at INTEGER NOT NULL,
                use_count    INTEGER NOT NULL DEFAULT 1,
                is_parent    INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO repo_bookmarks (repo_path, last_used_at) VALUES ('/repo/a', 1);
             CREATE TABLE repo_bookmarks_v39 (leftover TEXT);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let (host, path): (String, String) = conn
            .query_row("SELECT host, repo_path FROM repo_bookmarks", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((host.as_str(), path.as_str()), ("", "/repo/a"));
    }

    #[test]
    fn migrate_from_v39_adds_git_kind_columns() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v39 state: the host-keyed repo_bookmarks shape, pre-v40.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '39');
             CREATE TABLE repo_bookmarks (
                host         TEXT NOT NULL DEFAULT '',
                repo_path    TEXT NOT NULL,
                label        TEXT,
                last_used_at INTEGER NOT NULL,
                use_count    INTEGER NOT NULL DEFAULT 1,
                is_parent    INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (host, repo_path)
             );
             INSERT INTO repo_bookmarks (host, repo_path, last_used_at)
             VALUES ('ssh:devbox', '/repo/a', 1);",
        )
        .unwrap();

        migrate(&conn).unwrap();
        // Re-run is a no-op (guarded ALTERs).
        migrate(&conn).unwrap();

        // Legacy row survives with both new columns NULL (= unknown).
        let (is_git, parent_path): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT is_git, parent_path FROM repo_bookmarks WHERE repo_path = '/repo/a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(is_git, None);
        assert_eq!(parent_path, None);

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v26_adds_is_parent_column() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v26 state: a repo_bookmarks table without the is_parent column.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '26');
             CREATE TABLE repo_bookmarks (
                repo_path TEXT PRIMARY KEY, label TEXT,
                last_used_at INTEGER NOT NULL, use_count INTEGER NOT NULL DEFAULT 1);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let has_is_parent: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('repo_bookmarks') WHERE name='is_parent'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_is_parent, "is_parent column should be added at v27");

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v27_adds_related_session_column() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v27 state: an automation_runs table without related_session_id.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '27');
             CREATE TABLE automation_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, automation_id INTEGER NOT NULL,
                started_at INTEGER NOT NULL, status TEXT NOT NULL,
                detail TEXT NOT NULL DEFAULT '');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let has_column: bool = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('automation_runs') \
                 WHERE name='related_session_id'",
            )
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            has_column,
            "related_session_id column should be added at v28"
        );

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v31_adds_session_messages_table() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v31 state: metadata pinned to 31, no session_messages table.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '31');",
        )
        .unwrap();

        let has_before: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_messages'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!has_before);

        migrate(&conn).unwrap();

        let has_after: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_messages'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_after, "session_messages table should be created at v32");

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v27_adds_parent_session_id_column() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v27 state: a sessions table without the parent_session_id column.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '27');
             CREATE TABLE sessions (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                agent TEXT NOT NULL DEFAULT 'claude',
                backend_id TEXT NOT NULL DEFAULT '',
                backend_type TEXT NOT NULL DEFAULT 'tmux',
                agent_session_id TEXT, cwd TEXT,
                additional_dirs TEXT NOT NULL DEFAULT '',
                shell_backend_id TEXT, created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL, deleted_at INTEGER);
             INSERT INTO sessions (id, name, created_at, updated_at)
                VALUES ('s1', 'demo', 0, 0);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let has_parent: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='parent_session_id'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            has_parent,
            "parent_session_id column should be added at v30"
        );

        // Existing rows survive with a NULL parent.
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(parent.is_none());

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v33_adds_hook_status_columns() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v33 state: a sessions table without the hook-status columns.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '33');
             CREATE TABLE sessions (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                agent TEXT NOT NULL DEFAULT 'claude',
                backend_id TEXT NOT NULL DEFAULT '',
                backend_type TEXT NOT NULL DEFAULT 'tmux',
                agent_session_id TEXT, cwd TEXT,
                additional_dirs TEXT NOT NULL DEFAULT '',
                shell_backend_id TEXT, parent_session_id TEXT,
                display_order INTEGER, created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL, deleted_at INTEGER);
             INSERT INTO sessions (id, name, created_at, updated_at)
                VALUES ('s1', 'demo', 0, 0);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        for col in ["hook_state", "hook_state_at", "seen_at"] {
            let exists: bool = conn
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('sessions') WHERE name='{col}'"
                ))
                .unwrap()
                .exists([])
                .unwrap();
            assert!(exists, "{col} column should be added at v34");
        }

        // Existing rows survive with NULL hook state.
        let state: Option<String> = conn
            .query_row(
                "SELECT hook_state FROM sessions WHERE id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(state.is_none());

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v34_adds_external_index() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v34 state: a tasks table (with the v25 external columns) but no
        // idx_tasks_external index.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '34');
             CREATE TABLE tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'todo', source TEXT NOT NULL DEFAULT 'local',
                external_id TEXT, external_url TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                deleted_at INTEGER);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let exists: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_tasks_external'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(exists, "idx_tasks_external should be added at v35");

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_from_v35_adds_action_command() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v35 state: tasks + automations without action_command.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '35');
             CREATE TABLE tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'todo', created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL, deleted_at INTEGER);
             CREATE TABLE automations (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1, schedule_kind TEXT NOT NULL,
                schedule_spec TEXT NOT NULL, action_kind TEXT NOT NULL,
                prompt TEXT NOT NULL, created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        for table in ["tasks", "automations"] {
            let exists: bool = conn
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('{table}') WHERE name='action_command'"
                ))
                .unwrap()
                .exists([])
                .unwrap();
            assert!(exists, "{table}.action_command should be added at v36");
        }

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migration_failure_does_not_advance_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal v23 state whose v24 step is guaranteed to fail: a
        // `scheduled_commands` table present (so the legacy-migration branch
        // fires) but missing every column the INSERT…SELECT reads, so the
        // batch errors out partway through `migrate`.
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata (key, value) VALUES ('schema_version', '23');
             CREATE TABLE scheduled_commands (id INTEGER PRIMARY KEY);",
        )
        .unwrap();

        let result = migrate(&conn);
        assert!(
            result.is_err(),
            "a failing migration step must propagate its error"
        );

        // The stored version must stay un-advanced: the final
        // `UPDATE metadata SET schema_version` runs only after every step
        // succeeds, so a mid-migration failure leaves it at the prior value.
        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            version, "23",
            "schema_version must not advance when a migration step fails"
        );
    }

    #[test]
    fn schema_seeds_metadata() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());

        let counter: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'session_counter'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(counter, "0");
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
    }

    #[test]
    fn migrate_from_v22_drops_feature_tables_and_adds_agent_column() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        // Simulate an existing v22 database: recreate the feature tables and
        // roll the stored version back.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS roles (role_name TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS mcp_servers (server_name TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS skills (skill_name TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS profiles (profile_name TEXT PRIMARY KEY);",
        )
        .unwrap();
        conn.execute(
            "UPDATE metadata SET value = '22' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for dropped in ["roles", "mcp_servers", "skills", "profiles"] {
            assert!(
                !tables.contains(&dropped.to_string()),
                "{dropped} should be dropped"
            );
        }
        let has_agent: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'agent'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_agent, "sessions.agent column should exist");
    }

    #[test]
    fn sessions_have_no_model_column() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('sessions')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            !columns.iter().any(|c| c == "model"),
            "sessions.model should be dropped at v21, got columns: {columns:?}"
        );
    }

    #[test]
    fn migrate_drops_legacy_model_column() {
        // Simulate a v18-era DB that still has the `model` column, then run
        // initialize() and confirm v21 drops it.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO metadata(key, value) VALUES ('schema_version', '18');
             CREATE TABLE sessions (
                id                TEXT PRIMARY KEY,
                name              TEXT NOT NULL,
                role              TEXT NOT NULL DEFAULT 'developer',
                backend_id        TEXT NOT NULL DEFAULT '',
                backend_type      TEXT NOT NULL DEFAULT 'tmux',
                agent_session_id  TEXT,
                cwd               TEXT,
                additional_dirs   TEXT NOT NULL DEFAULT '',
                shell_backend_id  TEXT,
                model             TEXT,
                created_at        INTEGER NOT NULL,
                updated_at        INTEGER NOT NULL,
                deleted_at        INTEGER
             );
             INSERT INTO sessions(id, name, model, created_at, updated_at)
                VALUES ('s1', 'demo', 'sonnet', 0, 0);",
        )
        .unwrap();

        initialize(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('sessions')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!columns.iter().any(|c| c == "model"));

        let name: String = conn
            .query_row("SELECT name FROM sessions WHERE id = 's1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(name, "demo", "row content should survive the migration");
    }

    #[test]
    fn sessions_have_no_project_id() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        // Insert a session without project_id (which no longer exists)
        conn.execute(
            "INSERT INTO sessions (id, name, created_at, updated_at) \
             VALUES ('s1', 'test', 0, 0)",
            [],
        )
        .unwrap();

        let name: String = conn
            .query_row("SELECT name FROM sessions WHERE id = 's1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(name, "test");
    }

    #[test]
    fn add_column_if_absent_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a INTEGER);").unwrap();

        // First call adds the column.
        add_column_if_absent(&conn, "t", "b", "TEXT").unwrap();
        assert!(column_exists(&conn, "t", "b").unwrap());

        // Re-running is a no-op (column already present), not an error.
        add_column_if_absent(&conn, "t", "b", "TEXT").unwrap();

        // A missing table is a no-op too (a real upgrade never hits this).
        add_column_if_absent(&conn, "does_not_exist", "b", "TEXT").unwrap();
    }

    #[test]
    fn add_column_if_absent_propagates_real_error() {
        let conn = Connection::open_in_memory().unwrap();
        // A non-empty table: SQLite rejects adding a NOT NULL column with no
        // default. That is a genuine failure, not a benign re-run, so it must
        // propagate rather than being swallowed.
        conn.execute_batch("CREATE TABLE t (a INTEGER); INSERT INTO t (a) VALUES (1);")
            .unwrap();

        let result = add_column_if_absent(&conn, "t", "b", "TEXT NOT NULL");
        assert!(
            result.is_err(),
            "a real ALTER failure must propagate, not be swallowed"
        );
    }

    #[test]
    fn drop_column_if_present_is_idempotent_and_propagates() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a INTEGER, b TEXT);")
            .unwrap();

        // Present → dropped.
        drop_column_if_present(&conn, "t", "b").unwrap();
        assert!(!column_exists(&conn, "t", "b").unwrap());

        // Already gone, and missing table: both no-ops.
        drop_column_if_present(&conn, "t", "b").unwrap();
        drop_column_if_present(&conn, "does_not_exist", "b").unwrap();
    }

    #[test]
    fn rename_column_if_present_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (old_name TEXT);")
            .unwrap();

        // Present → renamed.
        rename_column_if_present(&conn, "t", "old_name", "new_name").unwrap();
        assert!(!column_exists(&conn, "t", "old_name").unwrap());
        assert!(column_exists(&conn, "t", "new_name").unwrap());

        // Source already gone, and missing table: both no-ops.
        rename_column_if_present(&conn, "t", "old_name", "new_name").unwrap();
        rename_column_if_present(&conn, "does_not_exist", "old_name", "new_name").unwrap();
    }
}
