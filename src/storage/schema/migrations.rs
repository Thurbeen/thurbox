//! The migration bodies, one per schema step.
//!
//! Split from the schema definition so a new migration is an append to this
//! file plus one row in `mod.rs`'s step table — the ordering contract stays
//! beside the DDL it migrates toward, and the forty bodies stop burying it.

use rusqlite::Connection;

use super::{
    add_column_if_absent, column_exists, drop_column_if_present, rename_column_if_present,
    table_exists,
};

/// v2 → v3: add additional_dirs column to sessions
pub(super) fn migrate_v3_additional_dirs(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(
        conn,
        "sessions",
        "additional_dirs",
        "TEXT NOT NULL DEFAULT ''",
    )
}

/// v3 → v4: add project_mcp_servers table
pub(super) fn migrate_v4_project_mcp_servers(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v5_session_commands(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v6_worktrees_pk(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v7_shell_backend_id(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "shell_backend_id", "TEXT")
}

/// v7 → v8: add env column to project_roles, add VM tables for sandboxed sessions
pub(super) fn migrate_v8_vms(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "project_roles", "env", "TEXT NOT NULL DEFAULT ''")?;

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
pub(super) fn migrate_v9_agent_session_id(conn: &Connection) -> rusqlite::Result<()> {
    rename_column_if_present(conn, "sessions", "claude_session_id", "agent_session_id")
}

/// v9 → v10: add containers and project_container_config tables
pub(super) fn migrate_v10_containers(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v11_containerfile(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "containers", "containerfile", "TEXT")?;
    add_column_if_absent(conn, "project_container_config", "containerfile", "TEXT")
}

/// v11 → v12: add scheduled_commands table for time-scheduled session inputs
pub(super) fn migrate_v12_scheduled_commands(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v13_roles(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v14_mcp_servers(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v15_nullable_project_id(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v16_drop_projects(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v17_skills(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v19_plugins(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v20_profiles(conn: &Connection) -> rusqlite::Result<()> {
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
/// removed from the product — the agent picks its own model now. The
/// drop is guarded so a re-run (column already gone) is a no-op.
pub(super) fn migrate_v21_drop_model(conn: &Connection) -> rusqlite::Result<()> {
    drop_column_if_present(conn, "sessions", "model")
}

/// v21 → v22: drop tables for removed subsystems. VM, devcontainer,
/// and process-plugin subsystems were removed to focus thurbox on
/// the TUI surface; the session_commands queue is unused now that
/// MCP `restart_session` / `create_session` run synchronously.
pub(super) fn migrate_v22_drop_subsystems(conn: &Connection) -> rusqlite::Result<()> {
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
pub(super) fn migrate_v23_generic_agent(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "agent", "TEXT NOT NULL DEFAULT 'claude'")?;
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
pub(super) fn migrate_v24_automations(conn: &Connection) -> rusqlite::Result<()> {
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
            next_run_at     INTEGER,
            action_extra_repos TEXT
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
/// Idempotent CREATE; no data backfill.
pub(super) fn migrate_v25_tasks(conn: &Connection) -> rusqlite::Result<()> {
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
/// ALTER, guarded so a re-run is a no-op.
pub(super) fn migrate_v26_task_description(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "tasks", "description", "TEXT")
}

/// v26 → v27: add an `is_parent` flag to `repo_bookmarks`. Parent bookmarks
/// store a folder whose immediate git sub-directories are re-scanned and listed
/// each time the repo picker opens.
///
/// Fresh v27 databases already have the column from `initialize` and skip this
/// step; existing v26 databases get it via the ALTER, guarded so a re-run is a
/// no-op.
pub(super) fn migrate_v27_repo_parent_bookmarks(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(
        conn,
        "repo_bookmarks",
        "is_parent",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

/// v28: typed related-session column on run history, replacing the
/// parse-the-detail-string approach (pre-v28 rows keep working via a
/// detail-parsing fallback in the TUI).
pub(super) fn migrate_v28_run_related_session(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "automation_runs", "related_session_id", "TEXT")
}

/// v29 → v30: add a nullable `parent_session_id` column to `sessions`
/// (lead/worker linkage for orchestration; v29 belongs to another branch).
///
/// Fresh v30 databases already have the column from `initialize` and skip this
/// step; existing databases get it via the ALTER, guarded so a re-run is a
/// no-op.
pub(super) fn migrate_v30_parent_session_id(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "parent_session_id", "TEXT")
}

/// v30 → v31: add a nullable `display_order` column to `sessions` (manual
/// position in the session list; `NULL` = never moved, renders after ordered
/// sessions in creation order). No backfill on purpose.
///
/// Fresh v31 databases already have the column from `initialize` and skip this
/// step; existing databases get it via the ALTER, guarded so a re-run is a
/// no-op.
pub(super) fn migrate_v31_display_order(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "display_order", "INTEGER")
}

/// v31 → v32: add the `session_messages` table (inter-session mailbox).
///
/// Idempotent CREATE: fresh v32 databases already have the table from
/// `initialize`; existing databases get it here. No data backfill.
pub(super) fn migrate_v32_session_messages(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_messages (
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
            ON session_messages(created_at);",
    )?;
    Ok(())
}

/// v32 → v33: add a nullable `action_extra_repos` column to `tasks` and
/// `automations`, storing the JSON list of additional repos a multi-repo
/// `Spawn` action spans. `NULL`/empty = a single-repo spawn (the common case),
/// so existing rows decode byte-identically to the pre-multi-repo behavior.
///
/// Fresh v33 databases already have the columns from `initialize` and skip this
/// step; existing databases get them via the ALTERs, guarded so a re-run is a
/// no-op.
pub(super) fn migrate_v33_action_extra_repos(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "tasks", "action_extra_repos", "TEXT")?;
    add_column_if_absent(conn, "automations", "action_extra_repos", "TEXT")
}

/// v33 → v34: add the hooks-driven session-status columns to `sessions`.
///
/// `hook_state` (`working`/`blocked`/`done`, NULL = no hook fired yet) and
/// `hook_state_at` (epoch ms it was reported) are written by
/// `thurbox-cli session signal` from an agent hook; `seen_at` (epoch ms) is
/// written by the TUI when the user views a `done` session, so it renders
/// `Idle` instead of `Done`. NULL on every existing row, so they decode
/// identically to the pre-hooks behaviour.
///
/// Fresh v34 databases already have the columns from `initialize` and skip this
/// step; existing databases get them via the ALTERs, guarded so a re-run is a
/// no-op.
pub(super) fn migrate_v34_hook_status(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "hook_state", "TEXT")?;
    add_column_if_absent(conn, "sessions", "hook_state_at", "INTEGER")?;
    add_column_if_absent(conn, "sessions", "seen_at", "INTEGER")
}

/// v34 → v35: index `tasks(source, external_id)` for external-tracker sync.
///
/// A task imported from an external tracker
/// look up a task by its `(source, external_id)` natural key on every sync tick
/// (`Database::get_task_by_external_id`) to dedup/upsert imported issues. The
/// columns exist since v25; this only adds the partial index (matching the
/// `idx_tasks_status` predicate so soft-deleted rows are excluded). Fresh v35
/// databases already have it from `initialize` and skip this step; it is a plain
/// (non-unique) index — dedup is enforced in application logic. `IF NOT EXISTS`
/// keeps a re-run a no-op.
pub(super) fn migrate_v35_tasks_external_index(conn: &Connection) -> rusqlite::Result<()> {
    // No-op when the table is absent (it is always created by an earlier step in
    // a real upgrade path); guard so this never errors on a partial schema.
    if !table_exists(conn, "tasks")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_tasks_external
            ON tasks(source, external_id) WHERE deleted_at IS NULL;",
    )
}

/// v35 → v36: add `action_command` to `tasks` + `automations`.
///
/// Holds the shell command for the `Exec` automation action (deterministic
/// scheduled jobs — the task-integration sync extensions). NULL on every
/// existing row, so non-Exec actions decode identically. Fresh v36 databases
/// already have the column from `initialize` and skip this step; the ALTERs add
/// it on upgrade (mirroring v33's `action_extra_repos`).
pub(super) fn migrate_v36_action_command(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "automations", "action_command", "TEXT")?;
    add_column_if_absent(conn, "tasks", "action_command", "TEXT")
}

/// v36 → v37: add `force_deleted` to `sessions`.
///
/// Marks a soft-deleted row whose runtime resources (tmux window + worktrees)
/// were torn down by a hard delete, so the `Ctrl+U` restore list can tag it and
/// refuse to restore it (its worktrees — and any uncommitted work — are gone).
/// `0` on every existing row, so prior soft-deletes stay restorable. Fresh v37
/// databases already have the column from `initialize` and skip this step.
pub(super) fn migrate_v37_force_deleted(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(
        conn,
        "sessions",
        "force_deleted",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

/// v37 → v38: the native code-review view. Adds `sessions.base_branch` (the
/// branch a worktree was forked from, so the review scopes to `<base>..HEAD`;
/// NULL on existing rows → review falls back to the repo's default branch) and
/// the `review_comments` / `review_marks` tables. Fresh v38 databases already
/// have all three from `initialize` and skip this step.
pub(super) fn migrate_v38_code_review(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "base_branch", "TEXT")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_comments (
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
        );",
    )?;
    Ok(())
}

/// v38 → v39: scope repo bookmarks to the **host** they live on. A remote
/// (SSH/WSL) target previously had no bookmark memory at all — the picker
/// opened empty — because a bookmark row couldn't say whose filesystem its
/// path belongs to. The primary key becomes `(host, repo_path)` (`''` =
/// local, else the backend name `ssh:<name>` / `wsl:<name>`), which needs a
/// table rebuild (SQLite can't alter a primary key). Existing rows migrate as
/// local. Guarded on the `host` column so a re-run is a no-op.
pub(super) fn migrate_v39_bookmark_host(conn: &Connection) -> rusqlite::Result<()> {
    if !table_exists(conn, "repo_bookmarks")? || column_exists(conn, "repo_bookmarks", "host")? {
        return Ok(());
    }
    // Transactional (unlike the PRAGMA-toggling rebuilds above, nothing here
    // forbids it): the version is only bumped after all steps, so a crash
    // mid-rebuild would otherwise strand a `repo_bookmarks_v39` orphan that
    // makes every re-run — and thus every DB open — fail. The DROP prefix
    // additionally repairs an orphan left by a pre-fix binary.
    conn.execute_batch(
        "BEGIN;
        DROP TABLE IF EXISTS repo_bookmarks_v39;
        CREATE TABLE repo_bookmarks_v39 (
            host         TEXT NOT NULL DEFAULT '',
            repo_path    TEXT NOT NULL,
            label        TEXT,
            last_used_at INTEGER NOT NULL,
            use_count    INTEGER NOT NULL DEFAULT 1,
            is_parent    INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (host, repo_path)
        );
        INSERT INTO repo_bookmarks_v39
            (host, repo_path, label, last_used_at, use_count, is_parent)
            SELECT '', repo_path, label, last_used_at, use_count, is_parent
            FROM repo_bookmarks;
        DROP TABLE repo_bookmarks;
        ALTER TABLE repo_bookmarks_v39 RENAME TO repo_bookmarks;
        COMMIT;",
    )
}

/// v39 → v40: teach `repo_bookmarks` what its path *is*. `is_git` (NULL =
/// unknown, for pre-v40 rows) records whether the path is a git repo — the
/// picker refuses the worktree toggle on a known non-repo, and remote rows
/// learn it opportunistically from listing/validation results instead of a
/// dedicated ssh round-trip. `parent_path` marks a row as a persisted child
/// of a remote parent bookmark (local parents keep the live re-scan; a remote
/// re-scan per picker open would be an ssh round-trip).
pub(super) fn migrate_v40_bookmark_git_kind(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "repo_bookmarks", "is_git", "INTEGER")?;
    add_column_if_absent(conn, "repo_bookmarks", "parent_path", "TEXT")
}

/// v40 → v41: the columns that make a session drivable by something other than
/// thurbox itself.
///
/// `launch_command`/`launch_args`/`launch_env` persist the **launch recipe** of
/// a session created from a raw command rather than an `agents.toml` entry.
/// They are the discriminant as well as the payload: `launch_command IS NULL`
/// means "a registry agent", which stays resolved **by reference** at restart
/// so fixing `agents.toml` and restarting still works. A command session has no
/// entry to re-resolve, so without these a restart would have nothing to replay
/// and its `--env` would evaporate on the first respawn.
///
/// `stopped_at` marks a session deliberately parked: the row and its checkout
/// are intact, only the pane is gone. It is a column rather than an absence
/// because "no window" is otherwise indistinguishable from damage, and three
/// subsystems repair damage on sight (extension self-heal, the TUI's respawn of
/// surveyed rows, `restart --if-missing`).
///
/// `session_meta` is per-session key/value for whoever is driving: a task id,
/// a lease, a correlation key. Namespaced by convention (`fm.*`, `gc.*`) so two
/// drivers against one database do not collide, and never interpreted here.
pub(super) fn migrate_v41_joinable(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(conn, "sessions", "launch_command", "TEXT")?;
    add_column_if_absent(conn, "sessions", "launch_args", "TEXT")?;
    add_column_if_absent(conn, "sessions", "launch_env", "TEXT")?;
    add_column_if_absent(conn, "sessions", "stopped_at", "INTEGER")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_meta (
            session_id TEXT NOT NULL REFERENCES sessions(id),
            key        TEXT NOT NULL,
            value      TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (session_id, key)
        );",
    )?;
    Ok(())
}

/// v41 → v42: record whether thurbox created each worktree
///
/// Defaults to 1: every worktree that predates the column was checked out by
/// thurbox itself (opening an existing one is what this column was added for),
/// so backfilling "mine" preserves force-delete's behavior for them exactly.
pub(super) fn migrate_v42_worktree_provenance(conn: &Connection) -> rusqlite::Result<()> {
    add_column_if_absent(
        conn,
        "worktrees",
        "created_by_thurbox",
        "INTEGER NOT NULL DEFAULT 1",
    )
}
