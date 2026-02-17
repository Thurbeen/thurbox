use rusqlite::Connection;

/// Current schema version. Incremented when schema changes.
pub const SCHEMA_VERSION: u32 = 3;

/// Create all tables and indexes if they don't exist.
pub fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projects (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            deleted_at INTEGER
        );

        CREATE TABLE IF NOT EXISTS project_repos (
            project_id TEXT NOT NULL REFERENCES projects(id),
            repo_path  TEXT NOT NULL,
            PRIMARY KEY (project_id, repo_path)
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id                TEXT PRIMARY KEY,
            name              TEXT NOT NULL,
            project_id        TEXT NOT NULL REFERENCES projects(id),
            role              TEXT NOT NULL DEFAULT 'developer',
            backend_id        TEXT NOT NULL DEFAULT '',
            backend_type      TEXT NOT NULL DEFAULT 'tmux',
            claude_session_id TEXT,
            cwd               TEXT,
            additional_dirs   TEXT NOT NULL DEFAULT '',
            created_at        INTEGER NOT NULL,
            updated_at        INTEGER NOT NULL,
            deleted_at        INTEGER
        );

        CREATE TABLE IF NOT EXISTS worktrees (
            session_id    TEXT PRIMARY KEY REFERENCES sessions(id),
            repo_path     TEXT NOT NULL,
            worktree_path TEXT NOT NULL,
            branch        TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            deleted_at    INTEGER
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
        CREATE INDEX IF NOT EXISTS idx_sessions_project
            ON sessions(project_id) WHERE deleted_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_sessions_active
            ON sessions(id) WHERE deleted_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_projects_active
            ON projects(id) WHERE deleted_at IS NULL;

        CREATE TABLE IF NOT EXISTS project_roles (
            project_id          TEXT NOT NULL REFERENCES projects(id),
            role_name           TEXT NOT NULL,
            description         TEXT NOT NULL DEFAULT '',
            permission_mode     TEXT,
            allowed_tools       TEXT NOT NULL DEFAULT '',
            disallowed_tools    TEXT NOT NULL DEFAULT '',
            tools               TEXT,
            append_system_prompt TEXT,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL,
            PRIMARY KEY (project_id, role_name)
        );
        ",
    )?;

    // Seed metadata if not present
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

    if version < 3 {
        // v2 → v3: add additional_dirs column to sessions
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN additional_dirs TEXT NOT NULL DEFAULT ''",
            [],
        );
        conn.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            [SCHEMA_VERSION.to_string()],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(tables.contains(&"projects".to_string()));
        assert!(tables.contains(&"project_repos".to_string()));
        assert!(tables.contains(&"project_roles".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"worktrees".to_string()));
        assert!(tables.contains(&"audit_log".to_string()));
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
        initialize(&conn).unwrap(); // Should not error
    }

    #[test]
    fn foreign_keys_enforced() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let result = conn.execute(
            "INSERT INTO sessions (id, name, project_id, created_at, updated_at) \
             VALUES ('s1', 'test', 'nonexistent', 0, 0)",
            [],
        );
        assert!(result.is_err());
    }
}
