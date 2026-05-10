use rusqlite::{params, Connection};

use crate::session::default_profiles;
use crate::sync::current_time_millis;

/// Current schema version. Incremented when schema changes.
pub const SCHEMA_VERSION: u32 = 21;

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

        CREATE TABLE IF NOT EXISTS sessions (
            id                TEXT PRIMARY KEY,
            name              TEXT NOT NULL,
            role              TEXT NOT NULL DEFAULT 'developer',
            backend_id        TEXT NOT NULL DEFAULT '',
            backend_type      TEXT NOT NULL DEFAULT 'tmux',
            agent_session_id  TEXT,
            cwd               TEXT,
            additional_dirs   TEXT NOT NULL DEFAULT '',
            shell_backend_id  TEXT,
            created_at        INTEGER NOT NULL,
            updated_at        INTEGER NOT NULL,
            deleted_at        INTEGER
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

        CREATE TABLE IF NOT EXISTS roles (
            role_name           TEXT PRIMARY KEY,
            description         TEXT NOT NULL DEFAULT '',
            permission_mode     TEXT,
            allowed_tools       TEXT NOT NULL DEFAULT '',
            disallowed_tools    TEXT NOT NULL DEFAULT '',
            tools               TEXT,
            append_system_prompt TEXT,
            env                 TEXT NOT NULL DEFAULT '',
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS mcp_servers (
            server_name TEXT PRIMARY KEY,
            command     TEXT NOT NULL DEFAULT '',
            args        TEXT NOT NULL DEFAULT '',
            env         TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS skills (
            skill_name TEXT PRIMARY KEY,
            path       TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS session_commands (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id   TEXT NOT NULL,
            command      TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            processed_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_session_commands_pending
            ON session_commands(id) WHERE processed_at IS NULL;

        CREATE TABLE IF NOT EXISTS vms (
            id          TEXT PRIMARY KEY,
            session_id  TEXT REFERENCES sessions(id),
            state       TEXT NOT NULL DEFAULT 'stopped',
            ssh_port    INTEGER NOT NULL,
            base_image  TEXT NOT NULL,
            cpus        INTEGER NOT NULL DEFAULT 2,
            memory_mb   INTEGER NOT NULL DEFAULT 2048,
            disk_gb     INTEGER NOT NULL DEFAULT 10,
            qemu_pid    INTEGER,
            error_msg   TEXT,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL,
            deleted_at  INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_vms_session
            ON vms(session_id) WHERE deleted_at IS NULL;

        CREATE TABLE IF NOT EXISTS containers (
            id                  TEXT PRIMARY KEY,
            session_id          TEXT REFERENCES sessions(id),
            state               TEXT NOT NULL DEFAULT 'stopped',
            docker_container_id TEXT,
            image               TEXT,
            cpus                INTEGER NOT NULL DEFAULT 2,
            memory_mb           INTEGER NOT NULL DEFAULT 2048,
            firewall_enabled    INTEGER NOT NULL DEFAULT 1,
            containerfile       TEXT,
            error_msg           TEXT,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL,
            deleted_at          INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_containers_session
            ON containers(session_id) WHERE deleted_at IS NULL;

        CREATE TABLE IF NOT EXISTS scheduled_commands (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id     TEXT NOT NULL,
            command_text   TEXT NOT NULL,
            scheduled_at   INTEGER NOT NULL,
            created_at     INTEGER NOT NULL,
            executed_at    INTEGER,
            cancelled_at   INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_scheduled_commands_pending
            ON scheduled_commands(scheduled_at)
            WHERE executed_at IS NULL AND cancelled_at IS NULL;

        CREATE TABLE IF NOT EXISTS repo_bookmarks (
            repo_path    TEXT PRIMARY KEY,
            label        TEXT,
            last_used_at INTEGER NOT NULL,
            use_count    INTEGER NOT NULL DEFAULT 1
        );

        CREATE TABLE IF NOT EXISTS plugins (
            plugin_name TEXT PRIMARY KEY,
            path        TEXT NOT NULL,
            version     TEXT NOT NULL DEFAULT '',
            enabled     INTEGER NOT NULL DEFAULT 1,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS plugin_settings (
            plugin_name TEXT NOT NULL,
            key         TEXT NOT NULL,
            value_json  TEXT NOT NULL,
            updated_at  INTEGER NOT NULL,
            PRIMARY KEY (plugin_name, key),
            FOREIGN KEY (plugin_name) REFERENCES plugins(plugin_name) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS profiles (
            profile_name      TEXT PRIMARY KEY,
            description       TEXT NOT NULL DEFAULT '',
            role_names        TEXT NOT NULL DEFAULT '',
            mcp_server_names  TEXT NOT NULL DEFAULT '',
            skill_names       TEXT NOT NULL DEFAULT '',
            created_at        INTEGER NOT NULL,
            updated_at        INTEGER NOT NULL
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
    seed_default_profiles(conn)?;

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
    }

    if version < 4 {
        // v3 → v4: add project_mcp_servers table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_mcp_servers (
                project_id  TEXT NOT NULL REFERENCES projects(id),
                server_name TEXT NOT NULL,
                command     TEXT NOT NULL DEFAULT '',
                args        TEXT NOT NULL DEFAULT '',
                env         TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (project_id, server_name)
            );",
        )?;
    }

    if version < 5 {
        // v4 → v5: add session_commands table for MCP-driven session operations
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_commands (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id   TEXT NOT NULL,
                command      TEXT NOT NULL,
                created_at   INTEGER NOT NULL,
                processed_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_session_commands_pending
                ON session_commands(id) WHERE processed_at IS NULL;",
        )?;
    }

    if version < 6 {
        // v5 → v6: change worktrees PK from session_id to (session_id, repo_path)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS worktrees_new (
                session_id    TEXT NOT NULL REFERENCES sessions(id),
                repo_path     TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                branch        TEXT NOT NULL,
                created_at    INTEGER NOT NULL,
                deleted_at    INTEGER,
                PRIMARY KEY (session_id, repo_path)
            );
            INSERT OR IGNORE INTO worktrees_new
                SELECT session_id, repo_path, worktree_path, branch, created_at, deleted_at
                FROM worktrees;
            DROP TABLE IF EXISTS worktrees;
            ALTER TABLE worktrees_new RENAME TO worktrees;",
        )?;
    }

    if version < 7 {
        // v6 → v7: add shell_backend_id column to sessions
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN shell_backend_id TEXT", []);
    }

    if version < 8 {
        // v7 → v8: add env column to project_roles, add VM tables for sandboxed sessions
        let _ = conn.execute(
            "ALTER TABLE project_roles ADD COLUMN env TEXT NOT NULL DEFAULT ''",
            [],
        );

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vms (
                id          TEXT PRIMARY KEY,
                session_id  TEXT REFERENCES sessions(id),
                project_id  TEXT REFERENCES projects(id),
                state       TEXT NOT NULL DEFAULT 'stopped',
                ssh_port    INTEGER NOT NULL,
                base_image  TEXT NOT NULL,
                cpus        INTEGER NOT NULL DEFAULT 2,
                memory_mb   INTEGER NOT NULL DEFAULT 2048,
                disk_gb     INTEGER NOT NULL DEFAULT 10,
                qemu_pid    INTEGER,
                error_msg   TEXT,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL,
                deleted_at  INTEGER
            );

            CREATE TABLE IF NOT EXISTS project_vm_config (
                project_id   TEXT PRIMARY KEY REFERENCES projects(id),
                base_image   TEXT,
                cpus         INTEGER,
                memory_mb    INTEGER,
                disk_gb      INTEGER,
                setup_script TEXT,
                updated_at   INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_vms_session
                ON vms(session_id) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_vms_project
                ON vms(project_id) WHERE deleted_at IS NULL;",
        )?;
    }

    if version < 9 {
        // v8 → v9: rename claude_session_id → agent_session_id
        let _ = conn.execute(
            "ALTER TABLE sessions RENAME COLUMN claude_session_id TO agent_session_id",
            [],
        );
    }

    if version < 10 {
        // v9 → v10: add containers and project_container_config tables
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS containers (
                id                  TEXT PRIMARY KEY,
                session_id          TEXT REFERENCES sessions(id),
                project_id          TEXT REFERENCES projects(id),
                state               TEXT NOT NULL DEFAULT 'stopped',
                docker_container_id TEXT,
                image               TEXT,
                cpus                INTEGER NOT NULL DEFAULT 2,
                memory_mb           INTEGER NOT NULL DEFAULT 2048,
                firewall_enabled    INTEGER NOT NULL DEFAULT 1,
                error_msg           TEXT,
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL,
                deleted_at          INTEGER
            );

            CREATE TABLE IF NOT EXISTS project_container_config (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id),
                image            TEXT,
                cpus             INTEGER,
                memory_mb        INTEGER,
                firewall_enabled INTEGER,
                updated_at       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_containers_session
                ON containers(session_id) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_containers_project
                ON containers(project_id) WHERE deleted_at IS NULL;",
        )?;
    }

    if version < 11 {
        // v10 → v11: add containerfile column to containers and project_container_config
        let _ = conn.execute("ALTER TABLE containers ADD COLUMN containerfile TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE project_container_config ADD COLUMN containerfile TEXT",
            [],
        );
    }

    if version < 12 {
        // v11 → v12: add scheduled_commands table for time-scheduled session inputs
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scheduled_commands (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id     TEXT NOT NULL,
                command_text   TEXT NOT NULL,
                scheduled_at   INTEGER NOT NULL,
                created_at     INTEGER NOT NULL,
                executed_at    INTEGER,
                cancelled_at   INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_scheduled_commands_pending
                ON scheduled_commands(scheduled_at)
                WHERE executed_at IS NULL AND cancelled_at IS NULL;",
        )?;
    }

    if version < 13 {
        // v12 → v13: add global `roles` table and seed from project_roles
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS roles (
                role_name           TEXT PRIMARY KEY,
                description         TEXT NOT NULL DEFAULT '',
                permission_mode     TEXT,
                allowed_tools       TEXT NOT NULL DEFAULT '',
                disallowed_tools    TEXT NOT NULL DEFAULT '',
                tools               TEXT,
                append_system_prompt TEXT,
                env                 TEXT NOT NULL DEFAULT '',
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO roles
                (role_name, description, permission_mode, allowed_tools,
                 disallowed_tools, tools, append_system_prompt, env,
                 created_at, updated_at)
                SELECT DISTINCT role_name, description, permission_mode, allowed_tools,
                       disallowed_tools, tools, append_system_prompt, env,
                       created_at, updated_at
                FROM project_roles;",
        )?;
    }

    if version < 14 {
        // v13 → v14: add global `mcp_servers` table and seed from project_mcp_servers
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_servers (
                server_name TEXT PRIMARY KEY,
                command     TEXT NOT NULL DEFAULT '',
                args        TEXT NOT NULL DEFAULT '',
                env         TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO mcp_servers
                (server_name, command, args, env, created_at, updated_at)
                SELECT DISTINCT server_name, command, args, env,
                       created_at, updated_at
                FROM project_mcp_servers;",
        )?;
    }

    if version < 15 {
        // v14 → v15: make project_id nullable on sessions, add repo_bookmarks table
        conn.execute_batch(
            "DROP TABLE IF EXISTS sessions_new;
            CREATE TABLE sessions_new (
                id                TEXT PRIMARY KEY,
                name              TEXT NOT NULL,
                project_id        TEXT REFERENCES projects(id),
                role              TEXT NOT NULL DEFAULT 'developer',
                backend_id        TEXT NOT NULL DEFAULT '',
                backend_type      TEXT NOT NULL DEFAULT 'tmux',
                agent_session_id  TEXT,
                cwd               TEXT,
                additional_dirs   TEXT NOT NULL DEFAULT '',
                shell_backend_id  TEXT,
                created_at        INTEGER NOT NULL,
                updated_at        INTEGER NOT NULL,
                deleted_at        INTEGER
            );
            PRAGMA foreign_keys = OFF;
            INSERT INTO sessions_new (id, name, project_id, role, backend_id,
                backend_type, agent_session_id, cwd, additional_dirs,
                shell_backend_id, created_at, updated_at, deleted_at)
                SELECT id, name,
                    CASE WHEN project_id IN (SELECT id FROM projects) THEN project_id ELSE NULL END,
                    role, backend_id,
                    backend_type, agent_session_id, cwd, additional_dirs,
                    shell_backend_id,
                    COALESCE(created_at, 0),
                    COALESCE(updated_at, 0),
                    deleted_at
                FROM sessions;
            DROP TABLE sessions;
            ALTER TABLE sessions_new RENAME TO sessions;
            PRAGMA foreign_keys = ON;
            CREATE INDEX IF NOT EXISTS idx_sessions_project
                ON sessions(project_id) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_sessions_active
                ON sessions(id) WHERE deleted_at IS NULL;

            CREATE TABLE IF NOT EXISTS repo_bookmarks (
                repo_path    TEXT PRIMARY KEY,
                label        TEXT,
                last_used_at INTEGER NOT NULL,
                use_count    INTEGER NOT NULL DEFAULT 1
            );

            INSERT OR IGNORE INTO repo_bookmarks (repo_path, last_used_at, use_count)
                SELECT DISTINCT repo_path,
                       COALESCE((SELECT MAX(s.updated_at) FROM sessions s
                                  INNER JOIN projects p ON p.id = s.project_id
                                  INNER JOIN project_repos pr ON pr.project_id = p.id
                                  AND pr.repo_path = project_repos.repo_path), 0),
                       1
                FROM project_repos;",
        )?;
    }

    if version < 16 {
        // v15 → v16: remove project tables and project_id columns from sessions/vms/containers
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;

            -- Recreate sessions without project_id
            DROP TABLE IF EXISTS sessions_new;
            CREATE TABLE sessions_new (
                id                TEXT PRIMARY KEY,
                name              TEXT NOT NULL,
                role              TEXT NOT NULL DEFAULT 'developer',
                backend_id        TEXT NOT NULL DEFAULT '',
                backend_type      TEXT NOT NULL DEFAULT 'tmux',
                agent_session_id  TEXT,
                cwd               TEXT,
                additional_dirs   TEXT NOT NULL DEFAULT '',
                shell_backend_id  TEXT,
                created_at        INTEGER NOT NULL,
                updated_at        INTEGER NOT NULL,
                deleted_at        INTEGER
            );
            INSERT INTO sessions_new (id, name, role, backend_id, backend_type,
                agent_session_id, cwd, additional_dirs, shell_backend_id,
                created_at, updated_at, deleted_at)
                SELECT id, name, role, backend_id, backend_type,
                    agent_session_id, cwd, additional_dirs, shell_backend_id,
                    created_at, updated_at, deleted_at
                FROM sessions;
            DROP TABLE sessions;
            ALTER TABLE sessions_new RENAME TO sessions;
            CREATE INDEX IF NOT EXISTS idx_sessions_active
                ON sessions(id) WHERE deleted_at IS NULL;

            -- Recreate vms without project_id
            DROP TABLE IF EXISTS vms_new;
            CREATE TABLE vms_new (
                id          TEXT PRIMARY KEY,
                session_id  TEXT REFERENCES sessions(id),
                state       TEXT NOT NULL DEFAULT 'stopped',
                ssh_port    INTEGER NOT NULL,
                base_image  TEXT NOT NULL,
                cpus        INTEGER NOT NULL DEFAULT 2,
                memory_mb   INTEGER NOT NULL DEFAULT 2048,
                disk_gb     INTEGER NOT NULL DEFAULT 10,
                qemu_pid    INTEGER,
                error_msg   TEXT,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL,
                deleted_at  INTEGER
            );
            INSERT INTO vms_new (id, session_id, state, ssh_port, base_image,
                cpus, memory_mb, disk_gb, qemu_pid, error_msg,
                created_at, updated_at, deleted_at)
                SELECT id, session_id, state, ssh_port, base_image,
                    cpus, memory_mb, disk_gb, qemu_pid, error_msg,
                    created_at, updated_at, deleted_at
                FROM vms;
            DROP TABLE IF EXISTS vms;
            ALTER TABLE vms_new RENAME TO vms;
            CREATE INDEX IF NOT EXISTS idx_vms_session
                ON vms(session_id) WHERE deleted_at IS NULL;

            -- Recreate containers without project_id
            DROP TABLE IF EXISTS containers_new;
            CREATE TABLE containers_new (
                id                  TEXT PRIMARY KEY,
                session_id          TEXT REFERENCES sessions(id),
                state               TEXT NOT NULL DEFAULT 'stopped',
                docker_container_id TEXT,
                image               TEXT,
                cpus                INTEGER NOT NULL DEFAULT 2,
                memory_mb           INTEGER NOT NULL DEFAULT 2048,
                firewall_enabled    INTEGER NOT NULL DEFAULT 1,
                containerfile       TEXT,
                error_msg           TEXT,
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL,
                deleted_at          INTEGER
            );
            INSERT INTO containers_new (id, session_id, state, docker_container_id,
                image, cpus, memory_mb, firewall_enabled, containerfile,
                error_msg, created_at, updated_at, deleted_at)
                SELECT id, session_id, state, docker_container_id,
                    image, cpus, memory_mb, firewall_enabled, containerfile,
                    error_msg, created_at, updated_at, deleted_at
                FROM containers;
            DROP TABLE IF EXISTS containers;
            ALTER TABLE containers_new RENAME TO containers;
            CREATE INDEX IF NOT EXISTS idx_containers_session
                ON containers(session_id) WHERE deleted_at IS NULL;

            -- Drop project tables
            DROP TABLE IF EXISTS project_container_config;
            DROP TABLE IF EXISTS project_vm_config;
            DROP TABLE IF EXISTS project_mcp_servers;
            DROP TABLE IF EXISTS project_roles;
            DROP TABLE IF EXISTS project_repos;
            DROP TABLE IF EXISTS projects;

            PRAGMA foreign_keys = ON;",
        )?;
    }

    if version < 17 {
        // v16 → v17: add skills table for global skill management
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS skills (
                skill_name TEXT PRIMARY KEY,
                path       TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )?;
    }

    if version < 19 {
        // v18 → v19: add plugins + plugin_settings tables for the plugin bundle system
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS plugins (
                plugin_name TEXT PRIMARY KEY,
                path        TEXT NOT NULL,
                version     TEXT NOT NULL DEFAULT '',
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS plugin_settings (
                plugin_name TEXT NOT NULL,
                key         TEXT NOT NULL,
                value_json  TEXT NOT NULL,
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (plugin_name, key),
                FOREIGN KEY (plugin_name) REFERENCES plugins(plugin_name) ON DELETE CASCADE
            );",
        )?;
    }

    if version < 20 {
        // v19 → v20: add profiles table bundling roles + MCP servers + skills
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS profiles (
                profile_name      TEXT PRIMARY KEY,
                description       TEXT NOT NULL DEFAULT '',
                role_names        TEXT NOT NULL DEFAULT '',
                mcp_server_names  TEXT NOT NULL DEFAULT '',
                skill_names       TEXT NOT NULL DEFAULT '',
                created_at        INTEGER NOT NULL,
                updated_at        INTEGER NOT NULL
            );",
        )?;
    }

    if version < 21 {
        // v20 → v21: drop the sessions.model column. Model selection was
        // removed from the product — the agent picks its own model now.
        // Use ignore-on-error in case an older SQLite (<3.35) lacks DROP
        // COLUMN support; the unused column is harmless.
        let _ = conn.execute("ALTER TABLE sessions DROP COLUMN model", []);
    }

    if version < SCHEMA_VERSION {
        conn.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            [SCHEMA_VERSION.to_string()],
        )?;
    }

    Ok(())
}

/// Seed the built-in profiles (see `session::default_profiles`) exactly once.
///
/// Guarded by the `profiles_seeded` metadata flag so admins who delete a
/// seeded profile don't see it resurrect on the next thurbox startup.
/// The flag is set on every call regardless of whether inserts happened,
/// which also handles the "v19 → v20 migration seeded the table, same
/// startup also calls this function" path — once the flag is set it stays
/// set.
fn seed_default_profiles(conn: &Connection) -> rusqlite::Result<()> {
    let already_seeded: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'profiles_seeded'",
            [],
            |row| row.get(0),
        )
        .ok();
    if already_seeded.is_some() {
        return Ok(());
    }

    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM profiles", [], |row| row.get(0))?;
    if existing == 0 {
        let now = current_time_millis() as i64;
        for profile in default_profiles() {
            conn.execute(
                "INSERT INTO profiles \
                 (profile_name, description, role_names, mcp_server_names, skill_names, \
                  created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    profile.name,
                    profile.description,
                    profile.roles.join(","),
                    profile.mcp_servers.join(","),
                    profile.skills.join(","),
                    now,
                    now,
                ],
            )?;
        }
    }

    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('profiles_seeded', '1')",
        [],
    )?;
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
        assert!(tables.contains(&"roles".to_string()));
        assert!(tables.contains(&"mcp_servers".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"worktrees".to_string()));
        assert!(tables.contains(&"audit_log".to_string()));
        assert!(tables.contains(&"session_commands".to_string()));
        assert!(tables.contains(&"containers".to_string()));
        assert!(tables.contains(&"scheduled_commands".to_string()));
        assert!(tables.contains(&"repo_bookmarks".to_string()));
        assert!(tables.contains(&"skills".to_string()));
        assert!(tables.contains(&"plugins".to_string()));
        assert!(tables.contains(&"plugin_settings".to_string()));
        assert!(tables.contains(&"profiles".to_string()));
        // Project tables should NOT exist
        assert!(!tables.contains(&"projects".to_string()));
        assert!(!tables.contains(&"project_repos".to_string()));
        assert!(!tables.contains(&"project_roles".to_string()));
        assert!(!tables.contains(&"project_mcp_servers".to_string()));
        assert!(!tables.contains(&"project_container_config".to_string()));
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
    fn schema_seeds_default_profiles_once() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT profile_name FROM profiles ORDER BY profile_name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            names,
            vec![crate::session::DEFAULT_PROFILE_ORCHESTRATOR.to_string()]
        );

        // Second run must not re-seed the deleted profile.
        conn.execute("DELETE FROM profiles", []).unwrap();
        initialize(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM profiles", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migrate_from_v19_adds_profiles_table_and_seeds() {
        let conn = Connection::open_in_memory().unwrap();
        // Bootstrap a v19 database by running initialize then rolling
        // the stored schema_version back to 19 and dropping the profiles
        // table to simulate an upgrade from an existing install. Also
        // clear the `profiles_seeded` flag since a genuine v19 DB would
        // never have set it.
        initialize(&conn).unwrap();
        conn.execute("DROP TABLE profiles", []).unwrap();
        conn.execute(
            "UPDATE metadata SET value = '19' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM metadata WHERE key = 'profiles_seeded'", [])
            .unwrap();

        initialize(&conn).unwrap();

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM profiles", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
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
}
