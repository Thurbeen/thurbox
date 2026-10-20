//! Persistence for [`Automation`]s and their run history.
//!
//! Schedules are stored as `(schedule_kind, schedule_spec)`; actions as
//! `(action_kind, …)` with the action-specific columns. The `next_run_at`
//! column is the dispatcher's scan key — see [`Database::claim_due_automation`].

use rusqlite::{params, OptionalExtension};

use crate::session::{
    Automation, AutomationAction, AutomationRun, AutomationRunStatus, AutomationSchedule,
    RunSettings, SessionId,
};
use crate::sync::current_time_millis;

use super::Database;

/// Fields needed to create an automation. `next_run_at` is computed by the
/// caller (it depends on the schedule + timezone, which live in `session`).
pub struct NewAutomation {
    pub name: String,
    pub enabled: bool,
    pub schedule: AutomationSchedule,
    pub timezone: Option<String>,
    pub action: AutomationAction,
    pub prompt: String,
    pub next_run_at: Option<u64>,
    /// Unique where set: an insert naming a key already taken fails with a
    /// constraint violation, which [`is_key_taken`] recognises.
    pub key: Option<String>,
    pub run: RunSettings,
}

/// How a run ended, as [`Database::finish_automation_run`] records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub status: AutomationRunStatus,
    /// The human summary: the error, the skip reason, an exec's output.
    pub detail: String,
    pub related_session: Option<SessionId>,
    pub finished_at: u64,
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
}

impl RunRecord {
    /// A run that produced no process: a send, a spawn, a skip.
    pub fn plain(
        status: AutomationRunStatus,
        detail: impl Into<String>,
        related_session: Option<SessionId>,
        finished_at: u64,
    ) -> Self {
        Self {
            status,
            detail: detail.into(),
            related_session,
            finished_at,
            exit_code: None,
            stdout_tail: None,
            stderr_tail: None,
        }
    }
}

/// Whether `error` is an insert refused because another automation holds its
/// key — the race two keyed creates of one key run.
pub fn is_key_taken(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(e, Some(message))
            if e.code == rusqlite::ErrorCode::ConstraintViolation
                && message.contains("automations.key")
    )
}

/// The `env` column: a JSON object in key order, `NULL` when empty.
fn env_to_column(env: &[(String, String)]) -> Option<String> {
    if env.is_empty() {
        return None;
    }
    let map: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    Some(serde_json::Value::Object(map).to_string())
}

/// Read the `env` column back, tolerating a malformed value as no variables.
fn env_from_column(raw: Option<String>) -> Vec<(String, String)> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    match serde_json::from_str::<std::collections::BTreeMap<String, String>>(&raw) {
        Ok(map) => map.into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

impl Database {
    /// Insert a new automation, returning its row id.
    pub fn create_automation(&self, new: &NewAutomation) -> rusqlite::Result<i64> {
        let now = current_time_millis() as i64;
        let (target_session, repo_path, worktree_branch, base_branch, agent, extra, command) =
            super::action_to_columns(&new.action);
        self.conn.execute(
            "INSERT INTO automations
                (name, enabled, schedule_kind, schedule_spec, timezone,
                 action_kind, target_session, repo_path, worktree_branch,
                 base_branch, agent, prompt, created_at, updated_at,
                 last_run_at, next_run_at, action_extra_repos, action_command,
                 key, timeout_secs, cwd, env)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, NULL, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20)",
            params![
                new.name,
                new.enabled as i64,
                new.schedule.kind(),
                new.schedule.spec(),
                new.timezone,
                new.action.kind(),
                target_session,
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                new.prompt,
                now,
                new.next_run_at.map(|v| v as i64),
                extra,
                command,
                new.key,
                new.run.timeout_secs.map(|v| v as i64),
                new.run.cwd.as_ref().map(|p| p.to_string_lossy().to_string()),
                env_to_column(&new.run.env),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The automation carrying `key`, if any.
    pub fn get_automation_by_key(&self, key: &str) -> rusqlite::Result<Option<Automation>> {
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM automations WHERE key = ?1"),
                params![key],
                map_automation,
            )
            .optional()
    }

    /// Every automation named `name`, oldest first. Names are not unique.
    pub fn find_automations_by_name(&self, name: &str) -> rusqlite::Result<Vec<Automation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLS} FROM automations WHERE name = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![name], map_automation)?;
        rows.collect()
    }

    /// Fetch a single automation by id.
    pub fn get_automation(&self, id: i64) -> rusqlite::Result<Option<Automation>> {
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM automations WHERE id = ?1"),
                params![id],
                map_automation,
            )
            .optional()
    }

    /// List all automations, newest first.
    pub fn list_automations(&self) -> rusqlite::Result<Vec<Automation>> {
        // Cached: the snapshot re-reads this on every refresh (ADR-P6).
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLS} FROM automations ORDER BY id DESC"))?;
        let rows = stmt.query_map([], map_automation)?;
        rows.collect()
    }

    /// All enabled automations whose `next_run_at` is due (`<= now`).
    pub fn due_automations(&self, now_millis: u64) -> rusqlite::Result<Vec<Automation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLS} FROM automations \
             WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
             ORDER BY next_run_at",
        ))?;
        let rows = stmt.query_map(params![now_millis as i64], map_automation)?;
        rows.collect()
    }

    /// Replace an automation's definition (everything except id/created_at).
    pub fn update_automation(&self, auto: &Automation) -> rusqlite::Result<()> {
        let (target_session, repo_path, worktree_branch, base_branch, agent, extra, command) =
            super::action_to_columns(&auto.action);
        let now = current_time_millis() as i64;
        self.conn.execute(
            "UPDATE automations SET
                name = ?2, enabled = ?3, schedule_kind = ?4, schedule_spec = ?5,
                timezone = ?6, action_kind = ?7, target_session = ?8, repo_path = ?9,
                worktree_branch = ?10, base_branch = ?11, agent = ?12, prompt = ?13,
                updated_at = ?14, last_run_at = ?15, next_run_at = ?16,
                action_extra_repos = ?17, action_command = ?18,
                timeout_secs = ?19, cwd = ?20, env = ?21
             WHERE id = ?1",
            params![
                auto.id,
                auto.name,
                auto.enabled as i64,
                auto.schedule.kind(),
                auto.schedule.spec(),
                auto.timezone,
                auto.action.kind(),
                target_session,
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                auto.prompt,
                now,
                auto.last_run_at.map(|v| v as i64),
                auto.next_run_at.map(|v| v as i64),
                extra,
                command,
                auto.run.timeout_secs.map(|v| v as i64),
                auto.run
                    .cwd
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                env_to_column(&auto.run.env),
            ],
        )?;
        Ok(())
    }

    /// [`Self::update_automation`] for a writer that read the row a moment ago.
    ///
    /// It never writes `last_run_at`, which only a fire writes. `timing` is the
    /// `(enabled, next_run_at)` the writer read: with `None` neither is written,
    /// and with `Some` both are written only if the row still holds them, so a
    /// fire the heartbeat claimed ([`Self::claim_due_automation`] is a
    /// compare-and-set on `next_run_at`) or a disable that landed meanwhile is
    /// not undone. Returns whether the row was written.
    pub fn update_automation_definition(
        &self,
        auto: &Automation,
        timing: Option<(bool, Option<u64>)>,
    ) -> rusqlite::Result<bool> {
        let (target_session, repo_path, worktree_branch, base_branch, agent, extra, command) =
            super::action_to_columns(&auto.action);
        let now = current_time_millis() as i64;
        let timeout = auto.run.timeout_secs.map(|v| v as i64);
        let cwd = auto
            .run
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let env = env_to_column(&auto.run.env);
        let definition = "name = ?2, schedule_kind = ?3, schedule_spec = ?4, timezone = ?5, \
             action_kind = ?6, target_session = ?7, repo_path = ?8, worktree_branch = ?9, \
             base_branch = ?10, agent = ?11, prompt = ?12, updated_at = ?13, \
             action_extra_repos = ?14, action_command = ?15, timeout_secs = ?16, cwd = ?17, \
             env = ?18";
        let columns = params![
            auto.id,
            auto.name,
            auto.schedule.kind(),
            auto.schedule.spec(),
            auto.timezone,
            auto.action.kind(),
            target_session,
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            auto.prompt,
            now,
            extra,
            command,
            timeout,
            cwd,
            env,
        ];
        let updated = match timing {
            Some((read_enabled, read_next)) => {
                let mut all = columns.to_vec();
                let enabled = auto.enabled as i64;
                let next = auto.next_run_at.map(|v| v as i64);
                let read_enabled = read_enabled as i64;
                let read_next = read_next.map(|v| v as i64);
                all.extend([
                    &enabled as &dyn rusqlite::ToSql,
                    &next,
                    &read_enabled,
                    &read_next,
                ]);
                self.conn.execute(
                    &format!(
                        "UPDATE automations SET {definition}, enabled = ?19, next_run_at = ?20 \
                         WHERE id = ?1 AND enabled = ?21 AND next_run_at IS ?22"
                    ),
                    all.as_slice(),
                )?
            }
            None => self.conn.execute(
                &format!("UPDATE automations SET {definition} WHERE id = ?1"),
                columns,
            )?,
        };
        Ok(updated > 0)
    }

    /// Switch an automation on with the next run computed from `read`, the
    /// row as the caller read it: only the flag and the next run are written,
    /// and only while the row is still off or unscheduled on that same schedule.
    /// An edit that landed meanwhile keeps its definition; one that moved the
    /// schedule is not enabled on the old one. Returns whether it was written.
    pub fn enable_automation(&self, read: &Automation, next_run_at: u64) -> rusqlite::Result<bool> {
        let updated = self.conn.execute(
            "UPDATE automations SET enabled = 1, next_run_at = ?2, updated_at = ?3 \
             WHERE id = ?1 AND (enabled = 0 OR next_run_at IS NULL) \
             AND schedule_kind = ?4 AND schedule_spec = ?5 AND timezone IS ?6",
            params![
                read.id,
                next_run_at as i64,
                current_time_millis() as i64,
                read.schedule.kind(),
                read.schedule.spec(),
                read.timezone,
            ],
        )?;
        Ok(updated > 0)
    }

    /// Enable or disable an automation. Disabling clears `next_run_at` so the
    /// due-scan skips it; enabling leaves `next_run_at` for the caller to set.
    pub fn set_automation_enabled(&self, id: i64, enabled: bool) -> rusqlite::Result<bool> {
        let now = current_time_millis() as i64;
        let updated = if enabled {
            self.conn.execute(
                "UPDATE automations SET enabled = 1, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?
        } else {
            self.conn.execute(
                "UPDATE automations SET enabled = 0, next_run_at = NULL, updated_at = ?2 \
                 WHERE id = ?1",
                params![id, now],
            )?
        };
        Ok(updated > 0)
    }

    /// Record that an automation fired: advance `last_run_at` and store the
    /// freshly computed `next_run_at` (`None` disables a spent one-shot).
    pub fn set_automation_next_run(
        &self,
        id: i64,
        last_run_at: u64,
        next_run_at: Option<u64>,
    ) -> rusqlite::Result<()> {
        // A one-shot with no further occurrence is also disabled.
        let enabled = next_run_at.is_some();
        self.conn.execute(
            "UPDATE automations SET last_run_at = ?2, next_run_at = ?3, enabled = \
             CASE WHEN ?4 THEN enabled ELSE 0 END, updated_at = ?5 WHERE id = ?1",
            params![
                id,
                last_run_at as i64,
                next_run_at.map(|v| v as i64),
                enabled as i64,
                current_time_millis() as i64,
            ],
        )?;
        Ok(())
    }

    /// Atomically claim a due automation for firing: advance its schedule **iff**
    /// `next_run_at` still equals `expected` (the value the caller observed as
    /// due). Returns `true` for the single winner; concurrent firers (a running
    /// TUI plus a headless `automation tick`) get `false` and must not fire.
    ///
    /// Claim-first ordering (advance, then fire) gives at-most-once semantics: a
    /// crash between claim and side effect loses a run rather than double-firing.
    /// `next` is the recomputed next occurrence (`None` disables a spent
    /// one-shot, mirroring [`set_automation_next_run`](Self::set_automation_next_run)).
    pub fn claim_due_automation(
        &self,
        id: i64,
        expected_next_run_at: u64,
        next: Option<u64>,
        now: u64,
    ) -> rusqlite::Result<bool> {
        let still_enabled = next.is_some();
        let updated = self.conn.execute(
            "UPDATE automations SET next_run_at = ?3, last_run_at = ?4, \
             enabled = CASE WHEN ?5 THEN enabled ELSE 0 END, updated_at = ?4 \
             WHERE id = ?1 AND next_run_at = ?2",
            params![
                id,
                expected_next_run_at as i64,
                next.map(|v| v as i64),
                now as i64,
                still_enabled as i64,
            ],
        )?;
        Ok(updated == 1)
    }

    /// Force an enabled automation to fire on the next dispatcher tick (manual
    /// run-now): set `next_run_at` to now. A disabled one is left alone and
    /// `false` returned — running it must not switch it back on.
    pub fn trigger_automation_now(&self, id: i64) -> rusqlite::Result<bool> {
        let now = current_time_millis() as i64;
        let updated = self.conn.execute(
            "UPDATE automations SET next_run_at = ?2, updated_at = ?2 \
             WHERE id = ?1 AND enabled = 1",
            params![id, now],
        )?;
        Ok(updated > 0)
    }

    /// Delete an automation and its run history.
    pub fn delete_automation(&self, id: i64) -> rusqlite::Result<bool> {
        self.conn.execute(
            "DELETE FROM automation_runs WHERE automation_id = ?1",
            params![id],
        )?;
        let updated = self
            .conn
            .execute("DELETE FROM automations WHERE id = ?1", params![id])?;
        Ok(updated > 0)
    }

    /// Disable every `send` automation that targets a session — used when a
    /// session is deleted, soft or forced, so nothing fires against a dead pane.
    pub fn disable_send_automations_for_session(
        &self,
        session_id: SessionId,
    ) -> rusqlite::Result<usize> {
        let now = current_time_millis() as i64;
        let updated = self.conn.execute(
            "UPDATE automations SET enabled = 0, next_run_at = NULL, updated_at = ?2 \
             WHERE action_kind = 'send' AND target_session = ?1",
            params![session_id.to_string(), now],
        )?;
        Ok(updated)
    }

    /// Append a run-history entry. `related_session` is the session the run
    /// sent to / spawned, when one exists.
    pub fn record_automation_run(
        &self,
        automation_id: i64,
        status: AutomationRunStatus,
        detail: &str,
        related_session: Option<SessionId>,
    ) -> rusqlite::Result<i64> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "INSERT INTO automation_runs \
             (automation_id, started_at, status, detail, related_session_id, finished_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?2)",
            params![
                automation_id,
                now,
                status.as_str(),
                detail,
                related_session.map(|id| id.to_string()),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Claim a due automation ([`Self::claim_due_automation`]) and open its
    /// run row in the same transaction, returning the row's id — `None` when
    /// another firer won the claim.
    ///
    /// One transaction, so no fire happens without a row and no row is opened
    /// for a fire that did not happen: a crash between the two used to leave a
    /// claimed slot with no trace. `deadline_at` is when a row still `running`
    /// counts as timed out ([`Self::time_out_stale_runs`]).
    pub fn claim_and_open_run(
        &self,
        id: i64,
        expected_next_run_at: u64,
        next: Option<u64>,
        now: u64,
        deadline_at: u64,
    ) -> rusqlite::Result<Option<i64>> {
        let tx = self.write_transaction()?;
        if !self.claim_due_automation(id, expected_next_run_at, next, now)? {
            return Ok(None);
        }
        let run = self.start_automation_run(id, now, Some(deadline_at))?;
        tx.commit()?;
        Ok(Some(run))
    }

    /// Open a run-history entry as `running`, stamped `started_at`, before
    /// the action fires; [`finish_automation_run`](Self::finish_automation_run)
    /// writes its outcome over it.
    pub fn start_automation_run(
        &self,
        automation_id: i64,
        started_at: u64,
        deadline_at: Option<u64>,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO automation_runs \
             (automation_id, started_at, status, detail, deadline_at) \
             VALUES (?1, ?2, ?3, '', ?4)",
            params![
                automation_id,
                started_at as i64,
                AutomationRunStatus::Running.as_str(),
                deadline_at.map(|v| v as i64),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record the outcome of a run opened by
    /// [`start_automation_run`](Self::start_automation_run).
    ///
    /// Written whatever the row says now, including `timed_out`: a firer that
    /// outlived its deadline and then returned knows better than the sweep
    /// that gave up on it.
    pub fn finish_automation_run(&self, run_id: i64, record: &RunRecord) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE automation_runs SET status = ?2, detail = ?3, related_session_id = ?4, \
             finished_at = ?5, exit_code = ?6, stdout_tail = ?7, stderr_tail = ?8 \
             WHERE id = ?1",
            params![
                run_id,
                record.status.as_str(),
                record.detail,
                record.related_session.map(|id| id.to_string()),
                record.finished_at as i64,
                record.exit_code,
                record.stdout_tail,
                record.stderr_tail,
            ],
        )?;
        Ok(())
    }

    /// Mark every run still `running` past its deadline as timed out, returning
    /// how many: a firer that hung past it or died leaves its row behind, and
    /// this is what says so.
    ///
    /// A row from before v56 has no deadline; one older than a day is past any
    /// run a heartbeat would still be waiting on.
    pub fn time_out_stale_runs(&self, now: u64) -> rusqlite::Result<usize> {
        const UNDATED_MS: i64 = 24 * 60 * 60 * 1000;
        self.conn.execute(
            "UPDATE automation_runs SET status = ?2, finished_at = ?1, \
             detail = 'timed out: still running past its deadline, so the run that \
             fired it hung or died' \
             WHERE status = ?3 AND ((deadline_at IS NOT NULL AND deadline_at < ?1) \
             OR (deadline_at IS NULL AND started_at < ?1 - ?4))",
            params![
                now as i64,
                AutomationRunStatus::TimedOut.as_str(),
                AutomationRunStatus::Running.as_str(),
                UNDATED_MS,
            ],
        )
    }

    /// One run by its id.
    pub fn get_automation_run(&self, run_id: i64) -> rusqlite::Result<Option<AutomationRun>> {
        self.conn
            .query_row(
                &format!("SELECT {RUN_COLS} FROM automation_runs WHERE id = ?1"),
                params![run_id],
                map_automation_run,
            )
            .optional()
    }

    /// List the most recent runs for an automation, newest first.
    pub fn list_automation_runs(
        &self,
        automation_id: i64,
        limit: u32,
    ) -> rusqlite::Result<Vec<AutomationRun>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM automation_runs \
             WHERE automation_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![automation_id, limit], map_automation_run)?;
        rows.collect()
    }

    /// The most recent runs of **every** automation in one query, keyed by
    /// automation id, newest first, at most `limit` each.
    ///
    /// The snapshot rebuild reads run history for the whole automation list;
    /// doing it via [`list_automation_runs`](Self::list_automation_runs) was a
    /// query per automation on every refresh. Ordered by `(automation_id,
    /// started_at DESC)` — `idx_automation_runs_automation` covers the grouping
    /// — and partitioned here, skipping rows past each automation's `limit`.
    pub fn list_recent_automation_runs(
        &self,
        limit: u32,
    ) -> rusqlite::Result<std::collections::HashMap<i64, Vec<AutomationRun>>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RUN_COLS} FROM automation_runs \
             ORDER BY automation_id, started_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map([], map_automation_run)?;

        let mut by_automation: std::collections::HashMap<i64, Vec<AutomationRun>> =
            std::collections::HashMap::new();
        for row in rows {
            let run = row?;
            let runs = by_automation.entry(run.automation_id).or_default();
            if runs.len() < limit as usize {
                runs.push(run);
            }
        }
        Ok(by_automation)
    }
}

/// Column list for run SELECTs (keep in sync with [`map_automation_run`]).
const RUN_COLS: &str = "id, automation_id, started_at, status, detail, related_session_id, \
    finished_at, exit_code, stdout_tail, stderr_tail";

fn map_automation_run(row: &rusqlite::Row) -> rusqlite::Result<AutomationRun> {
    Ok(AutomationRun {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        started_at: row.get::<_, i64>(2)? as u64,
        status: AutomationRunStatus::from_db(&row.get::<_, String>(3)?),
        detail: row.get(4)?,
        // Tolerate malformed ids — treat them as "no related session".
        related_session_id: row
            .get::<_, Option<String>>(5)?
            .and_then(|s| s.parse().ok()),
        finished_at: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        exit_code: row.get(7)?,
        stdout_tail: row.get(8)?,
        stderr_tail: row.get(9)?,
    })
}

/// Column list for automation SELECTs (keep in sync with [`map_automation`]).
const COLS: &str = "id, name, enabled, schedule_kind, schedule_spec, timezone, \
    action_kind, target_session, repo_path, worktree_branch, base_branch, agent, \
    prompt, created_at, updated_at, last_run_at, next_run_at, action_extra_repos, \
    action_command, key, timeout_secs, cwd, env";

fn map_automation(row: &rusqlite::Row) -> rusqlite::Result<Automation> {
    let id: i64 = row.get(0)?;
    let schedule_kind: String = row.get(3)?;
    let schedule_spec: String = row.get(4)?;
    let action_kind: String = row.get(6)?;
    let cols: super::ActionColumns = (
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(17)?,
        row.get(18)?,
    );

    let schedule =
        AutomationSchedule::from_parts(&schedule_kind, &schedule_spec).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                id as usize,
                rusqlite::types::Type::Text,
                format!("invalid schedule {schedule_kind}/{schedule_spec}").into(),
            )
        })?;

    let action = super::action_from_columns(&action_kind, cols);

    Ok(Automation {
        id,
        name: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        schedule,
        timezone: row.get(5)?,
        action,
        prompt: row.get(12)?,
        created_at: row.get::<_, i64>(13)? as u64,
        updated_at: row.get::<_, i64>(14)? as u64,
        last_run_at: row.get::<_, Option<i64>>(15)?.map(|v| v as u64),
        next_run_at: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
        key: row.get(19)?,
        run: RunSettings {
            timeout_secs: row.get::<_, Option<i64>>(20)?.map(|v| v as u64),
            cwd: row
                .get::<_, Option<String>>(21)?
                .map(std::path::PathBuf::from),
            env: env_from_column(row.get(22)?),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn send_automation(name: &str, next: Option<u64>) -> NewAutomation {
        NewAutomation {
            name: name.to_string(),
            enabled: true,
            schedule: AutomationSchedule::Once {
                at: next.unwrap_or(0),
            },
            timezone: None,
            action: AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "run tests".to_string(),
            next_run_at: next,
            key: None,
            run: RunSettings::default(),
        }
    }

    #[test]
    fn create_get_and_list_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("nightly", Some(123)))
            .unwrap();
        assert!(id > 0);

        let fetched = db.get_automation(id).unwrap().unwrap();
        assert_eq!(fetched.name, "nightly");
        assert_eq!(fetched.prompt, "run tests");
        assert_eq!(fetched.next_run_at, Some(123));
        assert!(matches!(fetched.action, AutomationAction::Send { .. }));

        assert_eq!(db.list_automations().unwrap().len(), 1);
    }

    #[test]
    fn spawn_action_columns_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let new = NewAutomation {
            name: "triage".into(),
            enabled: true,
            schedule: AutomationSchedule::Cron {
                expr: "0 9 * * 1-5".into(),
            },
            timezone: Some("Europe/Zurich".into()),
            action: AutomationAction::Spawn {
                repo_path: PathBuf::from("/tmp/repo"),
                worktree_branch: Some("feat/auto".into()),
                base_branch: Some("main".into()),
                agent: Some("codex".into()),
                extra_repos: Vec::new(),
            },
            prompt: "triage issues".into(),
            next_run_at: Some(999),
            key: None,
            run: RunSettings::default(),
        };
        let id = db.create_automation(&new).unwrap();
        let got = db.get_automation(id).unwrap().unwrap();
        assert_eq!(got.timezone.as_deref(), Some("Europe/Zurich"));
        assert!(matches!(got.schedule, AutomationSchedule::Cron { .. }));
        match got.action {
            AutomationAction::Spawn {
                repo_path,
                worktree_branch,
                base_branch,
                agent,
                extra_repos,
            } => {
                assert_eq!(repo_path, PathBuf::from("/tmp/repo"));
                assert_eq!(worktree_branch.as_deref(), Some("feat/auto"));
                assert_eq!(base_branch.as_deref(), Some("main"));
                assert_eq!(agent.as_deref(), Some("codex"));
                assert!(extra_repos.is_empty());
            }
            _ => panic!("expected spawn"),
        }
    }

    #[test]
    fn exec_action_columns_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let new = NewAutomation {
            name: "sync".into(),
            enabled: true,
            schedule: AutomationSchedule::Cron {
                expr: "*/15 * * * *".into(),
            },
            timezone: None,
            action: AutomationAction::Exec {
                command: "~/tracker-sync/sync.sh".into(),
            },
            prompt: String::new(),
            next_run_at: Some(42),
            key: None,
            run: RunSettings::default(),
        };
        let id = db.create_automation(&new).unwrap();
        let got = db.get_automation(id).unwrap().unwrap();
        match got.action {
            AutomationAction::Exec { command } => {
                assert_eq!(command, "~/tracker-sync/sync.sh");
            }
            other => panic!("expected exec, got {other:?}"),
        }
    }

    #[test]
    fn due_honors_enabled_and_next_run() {
        let db = Database::open_in_memory().unwrap();
        let due_id = db
            .create_automation(&send_automation("due", Some(100)))
            .unwrap();
        let _future = db
            .create_automation(&send_automation("future", Some(10_000)))
            .unwrap();
        let disabled = NewAutomation {
            enabled: false,
            ..send_automation("disabled", Some(100))
        };
        db.create_automation(&disabled).unwrap();

        let due = db.due_automations(500).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, due_id);
    }

    #[test]
    fn set_next_run_disables_spent_one_shot() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("once", Some(100)))
            .unwrap();
        db.set_automation_next_run(id, 200, None).unwrap();
        let got = db.get_automation(id).unwrap().unwrap();
        assert!(!got.enabled);
        assert_eq!(got.next_run_at, None);
        assert_eq!(got.last_run_at, Some(200));
        assert!(db.due_automations(10_000).unwrap().is_empty());
    }

    #[test]
    fn set_next_run_keeps_recurring_enabled() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("recurring", Some(100)))
            .unwrap();
        db.set_automation_next_run(id, 200, Some(5000)).unwrap();
        let got = db.get_automation(id).unwrap().unwrap();
        assert!(got.enabled);
        assert_eq!(got.next_run_at, Some(5000));
    }

    #[test]
    fn trigger_now_makes_due() {
        // A far-future bound that stays positive when cast to i64.
        let far_future = i64::MAX as u64;
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("manual", None))
            .unwrap();
        assert!(db.due_automations(far_future).unwrap().is_empty());
        assert!(db.trigger_automation_now(id).unwrap());
        assert_eq!(db.due_automations(far_future).unwrap().len(), 1);
    }

    #[test]
    fn claim_is_won_only_once() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("recurring", Some(100)))
            .unwrap();
        // First claim with the observed next_run_at wins and advances it.
        assert!(db.claim_due_automation(id, 100, Some(5000), 200).unwrap());
        // A second claim with the now-stale expected value loses.
        assert!(!db.claim_due_automation(id, 100, Some(9000), 200).unwrap());
        let got = db.get_automation(id).unwrap().unwrap();
        assert_eq!(got.next_run_at, Some(5000));
        assert_eq!(got.last_run_at, Some(200));
        assert!(got.enabled);
    }

    #[test]
    fn claim_with_none_disables_one_shot() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("once", Some(100)))
            .unwrap();
        assert!(db.claim_due_automation(id, 100, None, 200).unwrap());
        let got = db.get_automation(id).unwrap().unwrap();
        assert!(!got.enabled);
        assert_eq!(got.next_run_at, None);
        assert!(db.due_automations(i64::MAX as u64).unwrap().is_empty());
    }

    #[test]
    fn claim_with_wrong_expected_does_not_fire() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("x", Some(100)))
            .unwrap();
        assert!(!db.claim_due_automation(id, 999, Some(5000), 200).unwrap());
        let got = db.get_automation(id).unwrap().unwrap();
        assert_eq!(got.next_run_at, Some(100));
        assert_eq!(got.last_run_at, None);
    }

    #[test]
    fn disable_send_for_session() {
        let db = Database::open_in_memory().unwrap();
        let sid = SessionId::default();
        let new = NewAutomation {
            action: AutomationAction::Send { session_id: sid },
            ..send_automation("s", Some(100))
        };
        let id = db.create_automation(&new).unwrap();
        assert_eq!(db.disable_send_automations_for_session(sid).unwrap(), 1);
        assert!(!db.get_automation(id).unwrap().unwrap().enabled);
    }

    #[test]
    fn run_history_insert_and_list() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("h", Some(100)))
            .unwrap();
        db.record_automation_run(id, AutomationRunStatus::Success, "ok", None)
            .unwrap();
        db.record_automation_run(id, AutomationRunStatus::Skipped, "no session", None)
            .unwrap();
        let runs = db.list_automation_runs(id, 10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].status, AutomationRunStatus::Skipped);
    }

    #[test]
    fn run_history_roundtrips_related_session() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("h", Some(100)))
            .unwrap();
        let sid = SessionId::default();
        db.record_automation_run(id, AutomationRunStatus::Success, "sent", Some(sid))
            .unwrap();
        db.record_automation_run(id, AutomationRunStatus::Skipped, "no session", None)
            .unwrap();
        let runs = db.list_automation_runs(id, 10).unwrap();
        assert_eq!(runs[0].related_session_id, None);
        assert_eq!(runs[1].related_session_id, Some(sid));
    }

    #[test]
    fn recent_runs_batch_partitions_per_automation_and_honors_limit() {
        let db = Database::open_in_memory().unwrap();
        let a = db
            .create_automation(&send_automation("a", Some(100)))
            .unwrap();
        let b = db
            .create_automation(&send_automation("b", Some(100)))
            .unwrap();
        for n in 0..3 {
            db.record_automation_run(a, AutomationRunStatus::Success, &format!("a{n}"), None)
                .unwrap();
        }
        db.record_automation_run(b, AutomationRunStatus::Skipped, "b0", None)
            .unwrap();

        let batched = db.list_recent_automation_runs(2).unwrap();
        // Each automation's slice matches its per-automation read, so the
        // snapshot can swap one for the other without the rows changing.
        assert_eq!(batched[&a], db.list_automation_runs(a, 2).unwrap());
        assert_eq!(batched[&b], db.list_automation_runs(b, 2).unwrap());
        assert_eq!(batched[&a].len(), 2, "limit applies per automation");
        // An automation with no runs simply has no key.
        let c = db
            .create_automation(&send_automation("c", Some(100)))
            .unwrap();
        assert!(!db.list_recent_automation_runs(2).unwrap().contains_key(&c));
    }

    #[test]
    fn delete_removes_automation_and_runs() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("d", Some(100)))
            .unwrap();
        db.record_automation_run(id, AutomationRunStatus::Success, "ok", None)
            .unwrap();
        assert!(db.delete_automation(id).unwrap());
        assert!(db.get_automation(id).unwrap().is_none());
        assert!(db.list_automation_runs(id, 10).unwrap().is_empty());
    }

    #[test]
    fn a_claim_opens_its_run_row_or_nothing() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("r", Some(100)))
            .unwrap();
        let run = db
            .claim_and_open_run(id, 100, Some(5000), 200, 900)
            .unwrap()
            .expect("the claim is won");
        let row = db.get_automation_run(run).unwrap().expect("opened");
        assert_eq!(row.status, AutomationRunStatus::Running);
        assert_eq!(row.started_at, 200);
        assert_eq!(row.finished_at, None);
        // The loser opens nothing.
        assert_eq!(
            db.claim_and_open_run(id, 100, Some(9000), 200, 900)
                .unwrap(),
            None
        );
        assert_eq!(db.list_automation_runs(id, 10).unwrap().len(), 1);
    }

    #[test]
    fn a_run_past_its_deadline_is_timed_out_and_a_live_one_is_not() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("r", Some(100)))
            .unwrap();
        let stale = db.start_automation_run(id, 10, Some(50)).unwrap();
        let live = db.start_automation_run(id, 10, Some(500)).unwrap();
        let done = db.start_automation_run(id, 10, Some(50)).unwrap();
        db.finish_automation_run(
            done,
            &RunRecord::plain(AutomationRunStatus::Success, "ok", None, 20),
        )
        .unwrap();
        assert_eq!(db.time_out_stale_runs(100).unwrap(), 1);
        let stale = db.get_automation_run(stale).unwrap().unwrap();
        assert_eq!(stale.status, AutomationRunStatus::TimedOut);
        assert_eq!(stale.finished_at, Some(100));
        let live = db.get_automation_run(live).unwrap().unwrap();
        assert_eq!(live.status, AutomationRunStatus::Running);
        let done = db.get_automation_run(done).unwrap().unwrap();
        assert_eq!(done.status, AutomationRunStatus::Success);
    }

    #[test]
    fn a_finished_run_keeps_its_exit_code_and_both_tails() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .create_automation(&send_automation("r", Some(100)))
            .unwrap();
        let run = db.start_automation_run(id, 10, None).unwrap();
        db.finish_automation_run(
            run,
            &RunRecord {
                status: AutomationRunStatus::Error,
                detail: "exit 3".into(),
                related_session: None,
                finished_at: 40,
                exit_code: Some(3),
                stdout_tail: Some("out".into()),
                stderr_tail: Some("err".into()),
            },
        )
        .unwrap();
        let row = db.get_automation_run(run).unwrap().unwrap();
        assert_eq!(row.exit_code, Some(3));
        assert_eq!(row.stdout_tail.as_deref(), Some("out"));
        assert_eq!(row.stderr_tail.as_deref(), Some("err"));
        assert_eq!(row.duration_ms(), Some(30));
    }

    #[test]
    fn a_key_is_unique_and_the_run_settings_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let keyed = NewAutomation {
            key: Some("nightly".into()),
            run: RunSettings {
                timeout_secs: Some(30),
                cwd: Some(PathBuf::from("/srv/app")),
                env: vec![("A".into(), "1".into()), ("B".into(), "two".into())],
            },
            ..send_automation("k", Some(100))
        };
        let id = db.create_automation(&keyed).unwrap();
        let got = db.get_automation_by_key("nightly").unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.run, keyed.run);
        let again = db.create_automation(&NewAutomation {
            key: Some("nightly".into()),
            ..send_automation("k2", Some(100))
        });
        assert!(is_key_taken(&again.unwrap_err()));
        // Unkeyed rows never collide.
        db.create_automation(&send_automation("a", None)).unwrap();
        db.create_automation(&send_automation("a", None)).unwrap();
        assert_eq!(db.find_automations_by_name("a").unwrap().len(), 2);
    }
}
