//! Persistence for [`Automation`]s and their run history.
//!
//! Schedules are stored as `(schedule_kind, schedule_spec)`; actions as
//! `(action_kind, …)` with the action-specific columns. The `next_run_at`
//! column is the dispatcher's scan key — see `app::process_automations`.

use rusqlite::{params, OptionalExtension};

use crate::session::{
    Automation, AutomationAction, AutomationRun, AutomationRunStatus, AutomationSchedule, SessionId,
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
                 last_run_at, next_run_at, action_extra_repos, action_command)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, NULL, ?14, ?15, ?16)",
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
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
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
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLS} FROM automations ORDER BY id DESC"))?;
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
                action_extra_repos = ?17, action_command = ?18
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
            ],
        )?;
        Ok(())
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

    /// Force an automation to fire on the next dispatcher tick (manual run-now):
    /// set `next_run_at` to now and ensure it is enabled.
    pub fn trigger_automation_now(&self, id: i64) -> rusqlite::Result<bool> {
        let now = current_time_millis() as i64;
        let updated = self.conn.execute(
            "UPDATE automations SET enabled = 1, next_run_at = ?2, updated_at = ?2 WHERE id = ?1",
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
    /// session is force-deleted so its one-shots don't fire against a dead pane.
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
             (automation_id, started_at, status, detail, related_session_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
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

    /// List the most recent runs for an automation, newest first.
    pub fn list_automation_runs(
        &self,
        automation_id: i64,
        limit: u32,
    ) -> rusqlite::Result<Vec<AutomationRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, automation_id, started_at, status, detail, related_session_id \
             FROM automation_runs \
             WHERE automation_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![automation_id, limit], |row| {
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
            })
        })?;
        rows.collect()
    }
}

/// Column list for automation SELECTs (keep in sync with [`map_automation`]).
const COLS: &str = "id, name, enabled, schedule_kind, schedule_spec, timezone, \
    action_kind, target_session, repo_path, worktree_branch, base_branch, agent, \
    prompt, created_at, updated_at, last_run_at, next_run_at, action_extra_repos, \
    action_command";

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
                command: "~/github-issues/sync.sh".into(),
            },
            prompt: String::new(),
            next_run_at: Some(42),
        };
        let id = db.create_automation(&new).unwrap();
        let got = db.get_automation(id).unwrap().unwrap();
        match got.action {
            AutomationAction::Exec { command } => {
                assert_eq!(command, "~/github-issues/sync.sh");
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
}
