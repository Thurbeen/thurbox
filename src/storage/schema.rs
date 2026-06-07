use rusqlite::Connection;

/// Current schema version. Incremented when schema changes.
pub const SCHEMA_VERSION: u32 = 27;

/// A single migration step: applied when the stored version is below `target`.
type MigrationStep = (u32, fn(&Connection) -> rusqlite::Result<()>);

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
            agent             TEXT NOT NULL DEFAULT 'claude',
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
            next_run_at     INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_automations_due
            ON automations(next_run_at)
            WHERE enabled = 1 AND next_run_at IS NOT NULL;

        CREATE TABLE IF NOT EXISTS automation_runs (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            automation_id INTEGER NOT NULL,
            started_at    INTEGER NOT NULL,
            status        TEXT NOT NULL,
            detail        TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_automation_runs_automation
            ON automation_runs(automation_id, started_at);

        CREATE TABLE IF NOT EXISTS repo_bookmarks (
            repo_path    TEXT PRIMARY KEY,
            label        TEXT,
            last_used_at INTEGER NOT NULL,
            use_count    INTEGER NOT NULL DEFAULT 1,
            is_parent    INTEGER NOT NULL DEFAULT 0
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
            description     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_status
            ON tasks(status) WHERE deleted_at IS NULL;
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

/// Run an idempotent `ALTER TABLE` whose *only* expected failure is that the
/// change is already in place (column already added/dropped, or an older SQLite
/// that lacks `DROP COLUMN`). Those benign cases keep the migration idempotent;
/// any *other* error is surfaced via `tracing::warn!` rather than silently
/// discarded — important because `SCHEMA_VERSION` advances regardless, so a
/// genuinely failed `ALTER` would otherwise leave an inconsistent schema with no
/// trace in the log.
fn exec_idempotent_alter(conn: &Connection, sql: &str, benign: &[&str]) {
    if let Err(e) = conn.execute(sql, []) {
        let msg = e.to_string().to_lowercase();
        if !benign.iter().any(|needle| msg.contains(needle)) {
            tracing::warn!("schema migration: unexpected error running `{sql}`: {e}");
        }
    }
}

/// v2 → v3: add additional_dirs column to sessions
fn migrate_v3_additional_dirs(conn: &Connection) -> rusqlite::Result<()> {
    // Benign: the column already exists (migration re-run on a newer DB).
    exec_idempotent_alter(
        conn,
        "ALTER TABLE sessions ADD COLUMN additional_dirs TEXT NOT NULL DEFAULT ''",
        &["duplicate column name"],
    );
    Ok(())
}

/// v3 → v4: add project_mcp_servers table
fn migrate_v4_project_mcp_servers(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v4 → v5: add session_commands table for MCP-driven session operations
fn migrate_v5_session_commands(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v5 → v6: change worktrees PK from session_id to (session_id, repo_path)
fn migrate_v6_worktrees_pk(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v6 → v7: add shell_backend_id column to sessions
fn migrate_v7_shell_backend_id(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN shell_backend_id TEXT", []);
    Ok(())
}

/// v7 → v8: add env column to project_roles, add VM tables for sandboxed sessions
fn migrate_v8_vms(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v8 → v9: rename claude_session_id → agent_session_id
fn migrate_v9_agent_session_id(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute(
        "ALTER TABLE sessions RENAME COLUMN claude_session_id TO agent_session_id",
        [],
    );
    Ok(())
}

/// v9 → v10: add containers and project_container_config tables
fn migrate_v10_containers(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v10 → v11: add containerfile column to containers and project_container_config
fn migrate_v11_containerfile(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute("ALTER TABLE containers ADD COLUMN containerfile TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE project_container_config ADD COLUMN containerfile TEXT",
        [],
    );
    Ok(())
}

/// v11 → v12: add scheduled_commands table for time-scheduled session inputs
fn migrate_v12_scheduled_commands(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v12 → v13: add global `roles` table and seed from project_roles
fn migrate_v13_roles(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v13 → v14: add global `mcp_servers` table and seed from project_mcp_servers
fn migrate_v14_mcp_servers(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v14 → v15: make project_id nullable on sessions, add repo_bookmarks table
fn migrate_v15_nullable_project_id(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v15 → v16: remove project tables and project_id columns from sessions/vms/containers
fn migrate_v16_drop_projects(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v16 → v17: add skills table for global skill management
fn migrate_v17_skills(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS skills (
            skill_name TEXT PRIMARY KEY,
            path       TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    )
}

/// v18 → v19: add plugins + plugin_settings tables for the plugin bundle system
fn migrate_v19_plugins(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v19 → v20: add profiles table bundling roles + MCP servers + skills
fn migrate_v20_profiles(conn: &Connection) -> rusqlite::Result<()> {
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
    )
}

/// v20 → v21: drop the sessions.model column. Model selection was
/// removed from the product — the agent picks its own model now.
/// Use ignore-on-error in case an older SQLite (<3.35) lacks DROP
/// COLUMN support; the unused column is harmless.
fn migrate_v21_drop_model(conn: &Connection) -> rusqlite::Result<()> {
    // Benign: the column is already gone (re-run), or the SQLite build predates
    // `DROP COLUMN` (<3.35) and rejects the syntax — the unused column is
    // harmless either way.
    exec_idempotent_alter(
        conn,
        "ALTER TABLE sessions DROP COLUMN model",
        &["no such column", "near \"drop\"", "syntax error"],
    );
    Ok(())
}

/// v21 → v22: drop tables for removed subsystems. VM, devcontainer,
/// and process-plugin subsystems were removed to focus thurbox on
/// the TUI surface; the session_commands queue is unused now that
/// MCP `restart_session` / `create_session` run synchronously.
fn migrate_v22_drop_subsystems(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS plugin_settings;
         DROP TABLE IF EXISTS plugins;
         DROP TABLE IF EXISTS vms;
         DROP TABLE IF EXISTS containers;
         DROP TABLE IF EXISTS project_vm_config;
         DROP TABLE IF EXISTS project_container_config;
         DROP TABLE IF EXISTS session_commands;",
    )
}

/// v22 → v23: pivot to a generic per-session agent. Add the `agent`
/// column to sessions (existing rows default to "claude") and drop the
/// now-unused Claude-config tables. Existing sessions/worktrees are kept.
fn migrate_v23_generic_agent(conn: &Connection) -> rusqlite::Result<()> {
    let has_agent_col: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'agent'")?
        .exists([])?;
    if !has_agent_col {
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN agent TEXT NOT NULL DEFAULT 'claude';",
        )?;
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS profiles;
         DROP TABLE IF EXISTS skills;
         DROP TABLE IF EXISTS mcp_servers;
         DROP TABLE IF EXISTS roles;
         DELETE FROM metadata WHERE key = 'profiles_seeded';",
    )
}

/// v23 → v24: replace the one-shot `scheduled_commands` table with the
/// unified `automations` concept (one-shot OR recurring cron, send OR
/// spawn actions) plus a `automation_runs` history table. Pending
/// one-shot scheduled commands are migrated forward as `once`/`send`
/// automations; executed/cancelled ones are dropped with the table.
fn migrate_v24_automations(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS automations (
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
            next_run_at     INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_automations_due
            ON automations(next_run_at)
            WHERE enabled = 1 AND next_run_at IS NOT NULL;
        CREATE TABLE IF NOT EXISTS automation_runs (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            automation_id INTEGER NOT NULL,
            started_at    INTEGER NOT NULL,
            status        TEXT NOT NULL,
            detail        TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_automation_runs_automation
            ON automation_runs(automation_id, started_at);",
    )?;

    // Only migrate if the legacy table is present (fresh DBs created at v24
    // never had it).
    let has_legacy: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='scheduled_commands'")?
        .exists([])?;
    if has_legacy {
        conn.execute_batch(
            "INSERT INTO automations
                (name, enabled, schedule_kind, schedule_spec, timezone,
                 action_kind, target_session, prompt,
                 created_at, updated_at, last_run_at, next_run_at)
             SELECT
                'migrated-' || id, 1, 'once', CAST(scheduled_at AS TEXT), NULL,
                'send', session_id, command_text,
                created_at, created_at, NULL, scheduled_at
             FROM scheduled_commands
             WHERE executed_at IS NULL AND cancelled_at IS NULL;
             DROP TABLE IF EXISTS scheduled_commands;",
        )?;
    }
    Ok(())
}

/// v24 → v25: add the `tasks` table (todo list with agent-action linkage).
///
/// Idempotent CREATE: fresh v25 databases already have the table from
/// `initialize`; existing v24 databases get it here. No data backfill.
fn migrate_v25_tasks(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
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
            deleted_at      INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_status
            ON tasks(status) WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

/// v25 → v26: add a nullable `description` column to `tasks` (markdown notes).
///
/// Fresh v26 databases already have the column from `initialize` and skip this
/// step (the seeded version is current); existing v25 databases get it via the
/// ALTER. `let _` swallows the "duplicate column" error so a re-run is a no-op.
fn migrate_v26_task_description(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute("ALTER TABLE tasks ADD COLUMN description TEXT", []);
    Ok(())
}

/// v26 → v27: add an `is_parent` flag to `repo_bookmarks`. Parent bookmarks
/// store a folder whose immediate git sub-directories are re-scanned and listed
/// each time the repo picker opens.
///
/// Fresh v27 databases already have the column from `initialize` and skip this
/// step; existing v26 databases get it via the ALTER. `let _` swallows the
/// "duplicate column" error so a re-run is a no-op.
fn migrate_v27_repo_parent_bookmarks(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute(
        "ALTER TABLE repo_bookmarks ADD COLUMN is_parent INTEGER NOT NULL DEFAULT 0",
        [],
    );
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
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"worktrees".to_string()));
        assert!(tables.contains(&"audit_log".to_string()));
        assert!(tables.contains(&"automations".to_string()));
        assert!(tables.contains(&"automation_runs".to_string()));
        assert!(tables.contains(&"repo_bookmarks".to_string()));
        assert!(tables.contains(&"tasks".to_string()));
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
    fn exec_idempotent_alter_ignores_benign_duplicate_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a INTEGER);").unwrap();

        // First add succeeds; the column now exists.
        exec_idempotent_alter(
            &conn,
            "ALTER TABLE t ADD COLUMN b TEXT",
            &["duplicate column name"],
        );
        let has_b: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('t') WHERE name='b'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_b);

        // Re-running is a benign "duplicate column" — swallowed, no panic.
        exec_idempotent_alter(
            &conn,
            "ALTER TABLE t ADD COLUMN b TEXT",
            &["duplicate column name"],
        );
    }

    #[test]
    fn exec_idempotent_alter_swallows_unexpected_errors() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a INTEGER);").unwrap();

        // Operating on a missing table is NOT in the benign list, so the helper
        // logs a warning and continues rather than panicking (migrations must
        // not abort here — they only need the error to be visible in the log).
        exec_idempotent_alter(
            &conn,
            "ALTER TABLE does_not_exist ADD COLUMN b TEXT",
            &["duplicate column name"],
        );
    }
}
