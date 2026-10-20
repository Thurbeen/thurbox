//! Writing an automation: create, keyed create, edit, enable, run-now, delete.
//!
//! The interface's pane and `thurbox-cli automation` both write through here,
//! so a rule is checked once: a cron expression must parse, a timezone must
//! exist, a schedule must fire at least once more, and a moved schedule is
//! written by compare-and-set so a fire the heartbeat claimed meanwhile stands.

use std::path::PathBuf;

use crate::session::automation::{check_timezone, parse_trigger};
use crate::session::{Automation, AutomationAction, SessionId};
use crate::storage::automations::{is_key_taken, NewAutomation};
use crate::storage::Database;

/// The fields a create or an edit carries, each as the caller passed it.
///
/// Typed fields rather than a command line: a name or a prompt is whatever the
/// user typed, and none of it is ever read by a shell on its way to the
/// database. On an edit `None` leaves a field as it is; a create needs a name, a
/// trigger and exactly one target — `session` (send), `repo` (spawn) or
/// `command` (exec).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub name: Option<String>,
    /// `hourly` | `daily` | `weekdays` | `weekly` | `cron:<expr>` | `at:<unix_millis>`.
    pub trigger: Option<String>,
    /// `HH:MM`, for a preset trigger.
    pub time: Option<String>,
    /// 0..=7 (0 and 7 are Sunday), for the `weekly` preset. Kept as passed so
    /// a value out of that range is refused rather than read as the default.
    pub weekday: Option<i64>,
    /// An IANA name; empty means the system's own.
    pub timezone: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
    pub session: Option<String>,
    pub repo: Option<String>,
    /// The worktree branch a spawn creates or reuses.
    pub branch: Option<String>,
    pub base: Option<String>,
    pub agent: Option<String>,
    pub command: Option<String>,
    /// Seconds a run may take; `0` is refused.
    pub timeout_secs: Option<u64>,
    /// Where an exec runs; empty goes back to the default (home).
    pub cwd: Option<String>,
    /// An exec's variables; `Some` replaces the whole set, and an empty list
    /// clears it.
    pub env: Option<Vec<(String, String)>>,
}

/// What a keyed create does when the key is already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnExisting {
    /// Rewrite the existing automation in place: same id, same history.
    Update,
    /// Leave it as it is and answer with it.
    Keep,
    /// Refuse.
    #[default]
    Fail,
}

/// What a create did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    Created(i64),
    Updated(i64),
    Kept(i64),
}

impl Saved {
    pub fn id(self) -> i64 {
        match self {
            Saved::Created(id) | Saved::Updated(id) | Saved::Kept(id) => id,
        }
    }

    /// `created`, `updated` or `kept` — what `automation create --json` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Saved::Created(_) => "created",
            Saved::Updated(_) => "updated",
            Saved::Kept(_) => "kept",
        }
    }
}

/// Why a create wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The draft is wrong, or the database refused it.
    Invalid(String),
    /// The key is taken and the caller asked not to touch what holds it.
    Conflict(String),
}

impl From<String> for SaveError {
    fn from(message: String) -> Self {
        SaveError::Invalid(message)
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Invalid(m) | SaveError::Conflict(m) => f.write_str(m),
        }
    }
}

/// Create an automation from `draft`, or — with a key already taken — do what
/// `on_existing` says to the one holding it.
pub fn create(
    db: &Database,
    draft: &Draft,
    key: Option<&str>,
    on_existing: OnExisting,
    now: u64,
) -> Result<Saved, SaveError> {
    if let Some(key) = key {
        super::super::names::validate_key(key)?;
        // A reference is tried as an id before a key, so an all-digit key
        // would name some other automation everywhere it is used.
        if key.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "invalid key '{key}': a key of digits only reads as an automation id"
            )
            .into());
        }
    }
    if let Some(key) = key {
        if let Some(existing) = by_key(db, key)? {
            return settle(db, key, existing, draft, on_existing, now);
        }
    }
    let wanted = from_draft(db, draft, now)?;
    require_next_run(&wanted, now)?;
    let inserted = db.create_automation(&NewAutomation {
        name: wanted.name.clone(),
        enabled: wanted.enabled,
        schedule: wanted.schedule.clone(),
        timezone: wanted.timezone.clone(),
        action: wanted.action.clone(),
        prompt: wanted.prompt.clone(),
        next_run_at: wanted.next_run_at,
        key: key.map(str::to_string),
        run: wanted.run.clone(),
    });
    match (inserted, key) {
        (Ok(id), _) => Ok(Saved::Created(id)),
        // Another create of the same key landed between the lookup and the
        // insert: it is the existing one now.
        (Err(e), Some(key)) if is_key_taken(&e) => {
            let existing = by_key(db, key)?
                .ok_or_else(|| format!("the automation holding key '{key}' vanished"))?;
            settle(db, key, existing, draft, on_existing, now)
        }
        (Err(e), _) => Err(format!("create automation: {e}").into()),
    }
}

fn by_key(db: &Database, key: &str) -> Result<Option<Automation>, String> {
    db.get_automation_by_key(key)
        .map_err(|e| format!("get automation by key: {e}"))
}

/// A keyed create that found its key taken by `existing`. `keep` answers
/// before the draft is built, so a create repeated after its one-shot came
/// due is not refused for a schedule it is not going to write.
fn settle(
    db: &Database,
    key: &str,
    existing: Automation,
    draft: &Draft,
    on_existing: OnExisting,
    now: u64,
) -> Result<Saved, SaveError> {
    match on_existing {
        OnExisting::Fail => Err(SaveError::Conflict(format!(
            "automation #{} already has key '{key}'; pass --on-existing update to rewrite it \
             or --on-existing keep to leave it",
            existing.id
        ))),
        OnExisting::Keep => Ok(Saved::Kept(existing.id)),
        OnExisting::Update => {
            rewrite(db, &existing, from_draft(db, draft, now)?, now)?;
            Ok(Saved::Updated(existing.id))
        }
    }
}

/// Write `wanted` over `existing`, keeping its id, key, history and — when the
/// schedule, timezone and enabled flag did not move — its pending fire. Only a
/// moved schedule has to fire again.
fn rewrite(
    db: &Database,
    existing: &Automation,
    wanted: Automation,
    now: u64,
) -> Result<(), String> {
    let unmoved = timing_key(existing) == timing_key(&wanted) && existing.next_run_at.is_some();
    if !unmoved {
        require_next_run(&wanted, now)?;
    }
    let auto = Automation {
        id: existing.id,
        created_at: existing.created_at,
        last_run_at: existing.last_run_at,
        key: existing.key.clone(),
        next_run_at: if unmoved {
            existing.next_run_at
        } else {
            wanted.next_run_at
        },
        ..wanted
    };
    let timing = (!unmoved).then_some((existing.enabled, existing.next_run_at));
    let written = db
        .update_automation_definition(&auto, timing)
        .map_err(|e| format!("update automation: {e}"))?;
    if written {
        Ok(())
    } else {
        edit_outcome(db, existing.id)
    }
}

/// What a stored fire depends on: an edit that leaves these alone leaves the
/// next run alone, so a fire that is due — or a run-now — still happens.
fn timing_key(auto: &Automation) -> (crate::session::AutomationSchedule, Option<String>, bool) {
    (auto.schedule.clone(), auto.timezone.clone(), auto.enabled)
}

/// Edit automation `id`: change what the draft names and keep the rest — the
/// target included, extra repositories and all — recomputing the next run only
/// when the schedule, timezone or enabled flag moved.
pub fn edit(db: &Database, id: i64, draft: &Draft, now: u64) -> Result<(), String> {
    let weekday = weekday(draft)?;
    let mut auto = db
        .get_automation(id)
        .map_err(|e| format!("get automation: {e}"))?
        .ok_or_else(|| format!("automation #{id} no longer exists"))?;
    let before = timing_key(&auto);
    let read_timing = (auto.enabled, auto.next_run_at);

    // `time`/`weekday` only shape a preset, so alone they would be a silent
    // no-op. And an edit never retargets.
    if draft.trigger.is_none() && (draft.time.is_some() || weekday.is_some()) {
        return Err("time and weekday only apply with a trigger (--trigger), as a preset".into());
    }
    if draft.session.is_some()
        || draft.repo.is_some()
        || draft.branch.is_some()
        || draft.base.is_some()
        || draft.agent.is_some()
    {
        return Err("an edit keeps its target; create a new automation to change it".into());
    }
    if let Some(trigger) = &draft.trigger {
        auto.schedule = parse_trigger(trigger, draft.time.as_deref(), weekday)?;
    }
    if let Some(command) = &draft.command {
        match &mut auto.action {
            AutomationAction::Exec { command: current } if !command.trim().is_empty() => {
                *current = command.clone();
            }
            AutomationAction::Exec { .. } => return Err("the command must not be empty".into()),
            _ => return Err("only an exec automation runs a command".into()),
        }
    }
    apply_common(&mut auto, draft)?;

    let unmoved = before == timing_key(&auto) && auto.next_run_at.is_some();
    if !unmoved {
        auto.next_run_at = if auto.enabled {
            Some(next_run(&auto, now)?)
        } else {
            None
        };
    }
    let timing = (!unmoved).then_some(read_timing);
    let written = db
        .update_automation_definition(&auto, timing)
        .map_err(|e| format!("update automation: {e}"))?;
    if written {
        Ok(())
    } else {
        edit_outcome(db, id)
    }
}

/// The automation a create's draft describes, validated, with its next run —
/// `None` for an enabled schedule with no future occurrence, which only a
/// write that needs a new next run refuses ([`require_next_run`]).
fn from_draft(db: &Database, draft: &Draft, now: u64) -> Result<Automation, String> {
    let weekday = weekday(draft)?;
    let schedule = parse_trigger(
        draft
            .trigger
            .as_deref()
            .ok_or("an automation needs a trigger")?,
        draft.time.as_deref(),
        weekday,
    )?;
    let mut auto = Automation {
        id: 0,
        name: String::new(),
        enabled: true,
        schedule,
        timezone: None,
        action: draft_action(db, draft)?,
        prompt: String::new(),
        created_at: now,
        updated_at: now,
        last_run_at: None,
        next_run_at: None,
        key: None,
        run: Default::default(),
    };
    apply_common(&mut auto, draft)?;
    auto.next_run_at = auto
        .enabled
        .then(|| auto.schedule.next_after(now, auto.timezone.as_deref()))
        .flatten();
    Ok(auto)
}

/// Refuse an enabled automation that would never fire again.
fn require_next_run(auto: &Automation, now: u64) -> Result<(), String> {
    match auto.enabled {
        true => next_run(auto, now).map(drop),
        false => Ok(()),
    }
}

/// The fields a create and an edit set alike, and the checks they share.
fn apply_common(auto: &mut Automation, draft: &Draft) -> Result<(), String> {
    if let Some(name) = &draft.name {
        auto.name = name.trim().to_string();
    }
    if auto.name.is_empty() {
        return Err("an automation needs a name".into());
    }
    if let Some(timezone) = &draft.timezone {
        auto.timezone = (!timezone.is_empty()).then(|| timezone.clone());
    }
    if let Some(timezone) = &auto.timezone {
        check_timezone(timezone)?;
    }
    if let Some(prompt) = &draft.prompt {
        auto.prompt = prompt.clone();
    }
    let exec = matches!(auto.action, AutomationAction::Exec { .. });
    if !exec && auto.prompt.trim().is_empty() {
        return Err("the prompt must not be empty".into());
    }
    if let Some(enabled) = draft.enabled {
        auto.enabled = enabled;
    }
    auto.schedule.check()?;
    match draft.timeout_secs {
        Some(0) => return Err("the timeout must be at least one second".into()),
        Some(secs) => auto.run.timeout_secs = Some(secs),
        None => {}
    }
    if let Some(cwd) = &draft.cwd {
        if !exec && !cwd.is_empty() {
            return Err("only an exec automation runs in a directory".into());
        }
        auto.run.cwd = match cwd.is_empty() {
            true => None,
            false => Some(existing_dir(cwd)?),
        };
    }
    if let Some(env) = &draft.env {
        if !exec && !env.is_empty() {
            return Err("only an exec automation runs with an environment".into());
        }
        let mut env = env.clone();
        for (key, _) in &env {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(format!("not an environment variable name: {key:?}"));
            }
        }
        env.sort();
        env.dedup_by(|later, earlier| later.0 == earlier.0);
        auto.run.env = env;
    }
    Ok(())
}

/// `cwd` with a leading `~` expanded and made absolute against the directory
/// the writer runs in, refused unless it is a directory here. Absolute because
/// the heartbeat fires from wherever it was started: a relative path would
/// mean somewhere else on every fire. Refused because an automation runs on
/// this machine, and a typo is cheaper to hear about now than as a run that
/// failed at three in the morning.
fn existing_dir(cwd: &str) -> Result<PathBuf, String> {
    let mut path = crate::paths::expand_tilde(cwd);
    if path.is_relative() {
        let here = std::env::current_dir()
            .map_err(|e| format!("resolve the relative directory {cwd}: {e}"))?;
        path = here.join(path);
    }
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()));
    }
    Ok(path)
}

fn weekday(draft: &Draft) -> Result<Option<u32>, String> {
    match draft.weekday {
        Some(day) if !(0..=7).contains(&day) => Err(format!(
            "invalid weekday `{day}` (use 0=Sun..6=Sat, or 7=Sun)"
        )),
        Some(day) => Ok(Some(day as u32)),
        None => Ok(None),
    }
}

/// The action a new automation's draft names: exactly one of a session to
/// send to, a repository to spawn in, or a command to run.
fn draft_action(db: &Database, draft: &Draft) -> Result<AutomationAction, String> {
    match (&draft.session, &draft.repo, &draft.command) {
        (Some(session), None, None) => {
            let session_id: SessionId = session
                .parse()
                .map_err(|_| format!("not a session id: {session}"))?;
            db.get_session_by_id(session_id)
                .map_err(|e| format!("get session: {e}"))?
                .ok_or_else(|| format!("no session {session} to send to"))?;
            Ok(AutomationAction::Send { session_id })
        }
        (None, Some(repo), None) => Ok(AutomationAction::Spawn {
            repo_path: repo.into(),
            worktree_branch: draft.branch.clone(),
            base_branch: draft.base.clone(),
            agent: draft.agent.clone(),
            extra_repos: Vec::new(),
        }),
        (None, None, Some(command)) if command.trim().is_empty() => {
            Err("the command must not be empty".into())
        }
        (None, None, Some(command)) => Ok(AutomationAction::Exec {
            command: command.clone(),
        }),
        (None, None, None) => {
            Err("an automation needs a target: a session, a repository or a command".into())
        }
        _ => Err("name only one target: a session, a repository or a command".into()),
    }
}

/// When an enabled automation fires next, or why it never will.
pub(super) fn next_run(auto: &Automation, now: u64) -> Result<u64, String> {
    auto.schedule.check()?;
    auto.schedule
        .next_after(now, auto.timezone.as_deref())
        .ok_or_else(|| {
            format!(
                "`{}` never fires again; give it a schedule in the future",
                auto.schedule.trigger()
            )
        })
}

/// Why an edit wrote nothing: the row was deleted since it was read, or its
/// timing moved underneath an edit that meant to move it.
fn edit_outcome(db: &Database, id: i64) -> Result<(), String> {
    match db
        .get_automation(id)
        .map_err(|e| format!("get automation: {e}"))?
    {
        None => Err(format!("automation #{id} no longer exists")),
        Some(_) => Err(format!(
            "automation #{id} changed while it was being edited; reload it and try again"
        )),
    }
}

/// Switch automation `id` on or off. Disabling clears the next run, so
/// enabling computes it again.
pub fn set_enabled(db: &Database, id: i64, enabled: bool, now: u64) -> Result<(), String> {
    if !enabled {
        return match db
            .set_automation_enabled(id, false)
            .map_err(|e| format!("set enabled: {e}"))?
        {
            true => Ok(()),
            false => Err(format!("automation #{id} no longer exists")),
        };
    }
    let auto = db
        .get_automation(id)
        .map_err(|e| format!("get automation: {e}"))?
        .ok_or_else(|| format!("automation #{id} no longer exists"))?;
    enable_from(db, &auto, now)
}

/// Switch on the automation `read` describes, as read a moment ago.
///
/// Already on and scheduled: nothing to do, since recomputing would push a fire
/// that is due, or a run-now, to the next slot. Otherwise the flag and the next
/// run are written by compare-and-set; a miss means another write landed since
/// the read. One that left the row on and scheduled — a second enable of the
/// same row — is success; any other is reported.
pub(super) fn enable_from(db: &Database, read: &Automation, now: u64) -> Result<(), String> {
    if read.enabled && read.next_run_at.is_some() {
        return Ok(());
    }
    let next = next_run(read, now)?;
    if db
        .enable_automation(read, next)
        .map_err(|e| format!("enable automation: {e}"))?
    {
        return Ok(());
    }
    let id = read.id;
    let now_row = db
        .get_automation(id)
        .map_err(|e| format!("get automation: {e}"))?
        .ok_or_else(|| format!("automation #{id} no longer exists"))?;
    if now_row.enabled && now_row.next_run_at.is_some() {
        Ok(())
    } else {
        Err(format!(
            "automation #{id} changed while it was being enabled; try again"
        ))
    }
}

/// Mark automation `id` due, so the next tick — the heartbeat's, a timer's or
/// a hand-run one — fires it. A disabled one is refused rather than switched
/// on: running it once must not leave it firing on its schedule.
pub fn trigger(db: &Database, id: i64) -> Result<(), String> {
    if db
        .trigger_automation_now(id)
        .map_err(|e| format!("trigger: {e}"))?
    {
        return Ok(());
    }
    db.get_automation(id)
        .map_err(|e| format!("get automation: {e}"))?
        .ok_or_else(|| format!("automation #{id} no longer exists"))?;
    Err(format!("automation #{id} is disabled; enable it to run it"))
}

/// Delete automation `id` and its run history; `false` when there was none.
pub fn remove(db: &Database, id: i64) -> Result<bool, String> {
    db.delete_automation(id)
        .map_err(|e| format!("delete automation: {e}"))
}

#[cfg(test)]
mod tests {
    use super::Draft as AutomationDraft;
    use super::*;
    use crate::session::automation::{AutomationSchedule, ExtraRepo};

    /// What the pane's command path does with a draft.
    fn save_automation(
        db: &Database,
        id: Option<i64>,
        draft: &AutomationDraft,
        now: u64,
    ) -> Result<(), String> {
        match id {
            Some(id) => edit(db, id, draft, now),
            None => create(db, draft, None, OnExisting::Fail, now)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    /// What the pane's enable/disable/run/delete does.
    fn automation(
        db: &Database,
        id: i64,
        enabled: Option<bool>,
        run_now: bool,
        delete: bool,
    ) -> Result<(), String> {
        if delete {
            return remove(db, id).map(|_| ());
        }
        if let Some(enabled) = enabled {
            set_enabled(db, id, enabled, NOW)?;
        }
        if run_now {
            trigger(db, id)?;
        }
        Ok(())
    }

    // 2024-01-01 00:00:00 UTC, a Monday.
    const NOW: u64 = 1_704_067_200_000;

    fn spawn_draft() -> AutomationDraft {
        AutomationDraft {
            name: Some("nightly".into()),
            trigger: Some("daily".into()),
            time: Some("09:30".into()),
            timezone: Some("UTC".into()),
            prompt: Some("review the queue".into()),
            repo: Some("/srv/app".into()),
            branch: Some("feat/nightly".into()),
            base: Some("main".into()),
            agent: Some("claude".into()),
            ..AutomationDraft::default()
        }
    }

    /// A run of a row that is gone says so, rather than calling it disabled.
    #[test]
    fn running_a_deleted_automation_says_it_no_longer_exists() {
        let db = Database::open_in_memory().expect("db");
        let error = automation(&db, 404, None, true, false).unwrap_err();
        assert_eq!(error, "automation #404 no longer exists");
    }

    fn only(db: &Database) -> crate::session::Automation {
        let mut all = db.list_automations().expect("list");
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    #[test]
    fn a_created_automation_is_stored_with_its_next_run() {
        let db = Database::open_in_memory().expect("db");
        save_automation(&db, None, &spawn_draft(), NOW).expect("create");
        let auto = only(&db);
        assert_eq!(auto.name, "nightly");
        assert!(auto.enabled);
        assert_eq!(
            auto.schedule,
            AutomationSchedule::Cron {
                expr: "30 9 * * *".into()
            }
        );
        assert_eq!(auto.timezone.as_deref(), Some("UTC"));
        assert_eq!(auto.prompt, "review the queue");
        assert_eq!(
            auto.action,
            AutomationAction::Spawn {
                repo_path: "/srv/app".into(),
                worktree_branch: Some("feat/nightly".into()),
                base_branch: Some("main".into()),
                agent: Some("claude".into()),
                extra_repos: Vec::new(),
            }
        );
        assert_eq!(auto.next_run_at, Some(NOW + (9 * 60 + 30) * 60_000));
    }

    #[test]
    fn a_disabled_create_is_not_scheduled() {
        let db = Database::open_in_memory().expect("db");
        let draft = AutomationDraft {
            enabled: Some(false),
            ..spawn_draft()
        };
        save_automation(&db, None, &draft, NOW).expect("create");
        let auto = only(&db);
        assert!(!auto.enabled);
        assert_eq!(auto.next_run_at, None);
    }

    #[test]
    fn an_exec_automation_needs_a_command_and_no_prompt() {
        let db = Database::open_in_memory().expect("db");
        let draft = AutomationDraft {
            name: Some("sync".into()),
            trigger: Some("cron:*/15 * * * *".into()),
            command: Some("make sync".into()),
            ..AutomationDraft::default()
        };
        save_automation(&db, None, &draft, NOW).expect("create");
        assert_eq!(
            only(&db).action,
            AutomationAction::Exec {
                command: "make sync".into()
            }
        );
    }

    #[test]
    fn a_create_names_what_is_wrong_and_stores_nothing() {
        let db = Database::open_in_memory().expect("db");
        let cases: Vec<(AutomationDraft, &str)> = vec![
            (
                AutomationDraft {
                    name: Some("  ".into()),
                    ..spawn_draft()
                },
                "name",
            ),
            (
                AutomationDraft {
                    trigger: Some("monthly".into()),
                    ..spawn_draft()
                },
                "unknown trigger",
            ),
            (
                AutomationDraft {
                    trigger: Some("cron:61 * * * *".into()),
                    time: None,
                    ..spawn_draft()
                },
                "cron",
            ),
            (
                AutomationDraft {
                    time: Some("25:00".into()),
                    ..spawn_draft()
                },
                "time",
            ),
            (
                AutomationDraft {
                    timezone: Some("Mars/Olympus".into()),
                    ..spawn_draft()
                },
                "timezone",
            ),
            (
                AutomationDraft {
                    trigger: Some(format!("at:{}", NOW - 1)),
                    time: None,
                    ..spawn_draft()
                },
                "never fires",
            ),
            (
                AutomationDraft {
                    prompt: Some(String::new()),
                    ..spawn_draft()
                },
                "prompt",
            ),
            (
                AutomationDraft {
                    repo: None,
                    branch: None,
                    base: None,
                    agent: None,
                    ..spawn_draft()
                },
                "target",
            ),
            (
                AutomationDraft {
                    command: Some("make sync".into()),
                    ..spawn_draft()
                },
                "only one",
            ),
            (
                AutomationDraft {
                    repo: None,
                    session: Some("not-a-session".into()),
                    ..spawn_draft()
                },
                "session",
            ),
            (
                AutomationDraft {
                    repo: None,
                    session: Some("6f1c1d8e-2a2b-4c1e-9d1f-0a1b2c3d4e5f".into()),
                    ..spawn_draft()
                },
                "no session",
            ),
        ];
        for (draft, expected) in cases {
            let error = save_automation(&db, None, &draft, NOW)
                .expect_err(&format!("{draft:?} must be refused"));
            assert!(
                error.contains(expected),
                "{draft:?}: {error:?} does not mention {expected:?}"
            );
        }
        assert!(db.list_automations().expect("list").is_empty());
    }

    fn stored_multi_repo(db: &Database) -> i64 {
        db.create_automation(&NewAutomation {
            name: "multi".into(),
            enabled: true,
            schedule: AutomationSchedule::Cron {
                expr: "0 9 * * 1-5".into(),
            },
            timezone: None,
            action: AutomationAction::Spawn {
                repo_path: "/srv/app".into(),
                worktree_branch: Some("feat/x".into()),
                base_branch: None,
                agent: None,
                extra_repos: vec![ExtraRepo {
                    repo_path: "/srv/lib".into(),
                    worktree: true,
                    base_branch: None,
                }],
            },
            prompt: "go".into(),
            next_run_at: Some(NOW + 1),
            key: None,
            run: Default::default(),
        })
        .expect("insert")
    }

    #[test]
    fn an_edit_changes_what_it_names_and_keeps_the_target() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        let before = only(&db).action;
        let edit = AutomationDraft {
            name: Some("renamed".into()),
            trigger: Some("cron:0 18 * * *".into()),
            timezone: Some("Europe/Zurich".into()),
            prompt: Some("wrap up".into()),
            ..AutomationDraft::default()
        };
        save_automation(&db, Some(id), &edit, NOW).expect("edit");
        let after = only(&db);
        assert_eq!(after.name, "renamed");
        assert_eq!(after.prompt, "wrap up");
        assert_eq!(after.timezone.as_deref(), Some("Europe/Zurich"));
        assert_eq!(after.action, before, "the target and its extra repos stay");
        assert!(after.next_run_at.is_some_and(|next| next > NOW));

        // An empty timezone goes back to the system's own.
        let clear = AutomationDraft {
            timezone: Some(String::new()),
            ..AutomationDraft::default()
        };
        save_automation(&db, Some(id), &clear, NOW).expect("edit");
        assert_eq!(only(&db).timezone, None);
    }

    #[test]
    fn an_edit_of_a_deleted_automation_says_so() {
        let db = Database::open_in_memory().expect("db");
        let error = save_automation(
            &db,
            Some(41),
            &AutomationDraft {
                name: Some("x".into()),
                ..AutomationDraft::default()
            },
            NOW,
        )
        .expect_err("nothing to edit");
        assert!(error.contains("#41"), "{error}");
    }

    #[test]
    fn only_an_exec_automation_takes_a_new_command() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        let error = save_automation(
            &db,
            Some(id),
            &AutomationDraft {
                command: Some("rm -rf /".into()),
                ..AutomationDraft::default()
            },
            NOW,
        )
        .expect_err("a spawn has no command");
        assert!(error.contains("command"), "{error}");

        let exec = AutomationDraft {
            name: Some("sync".into()),
            trigger: Some("hourly".into()),
            command: Some("make sync".into()),
            ..AutomationDraft::default()
        };
        save_automation(&db, None, &exec, NOW).expect("create");
        let exec_id = db
            .list_automations()
            .expect("list")
            .into_iter()
            .find(|a| a.name == "sync")
            .expect("created")
            .id;
        save_automation(
            &db,
            Some(exec_id),
            &AutomationDraft {
                command: Some("make sync-all".into()),
                ..AutomationDraft::default()
            },
            NOW,
        )
        .expect("edit");
        let edited = db.get_automation(exec_id).expect("get").expect("row");
        assert_eq!(
            edited.action,
            AutomationAction::Exec {
                command: "make sync-all".into()
            }
        );
    }

    /// Each command runs on its own worker, so an enable can read a row an
    /// edit then rewrites. The enable must write the flag and the next run
    /// only, and only for the schedule it computed that run from.
    #[test]
    fn an_enable_racing_an_edit_does_not_undo_it() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        automation(&db, id, Some(false), false, false).expect("disable");
        let stale = db.get_automation(id).expect("get").expect("row");
        // The edit lands between the enable's read and its write.
        let mut edited = stale.clone();
        edited.name = "renamed".into();
        db.update_automation_definition(&edited, None)
            .expect("rename");
        assert!(db.enable_automation(&stale, NOW + 1).expect("enable"));
        let row = only(&db);
        assert_eq!(row.name, "renamed", "the edit stands");
        assert!(row.enabled);
        assert_eq!(row.next_run_at, Some(NOW + 1));

        // A schedule that moved underneath is not enabled on the old one.
        automation(&db, id, Some(false), false, false).expect("disable");
        let stale = db.get_automation(id).expect("get").expect("row");
        let mut moved = stale.clone();
        moved.schedule = AutomationSchedule::Cron {
            expr: "0 18 * * *".into(),
        };
        db.update_automation_definition(&moved, None)
            .expect("reschedule");
        assert!(!db.enable_automation(&stale, NOW + 1).expect("enable"));
        assert!(!only(&db).enabled);
    }

    /// Two enables sent before the snapshot shows the first (a double
    /// press) both find the row off; the second's compare-and-set misses,
    /// and it must report the automation on rather than an error.
    #[test]
    fn a_second_enable_of_the_same_row_is_not_an_error() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        automation(&db, id, Some(false), false, false).expect("disable");
        let stale = db.get_automation(id).expect("get").expect("row");
        // Both enables read the row while it was off.
        enable_from(&db, &stale, NOW).expect("the first enable");
        enable_from(&db, &stale, NOW).expect("the second finds it already on");
        assert!(only(&db).enabled);
    }

    /// A delete that lands between an edit's read and its write leaves
    /// nothing to reload: the edit says the automation is gone.
    #[test]
    fn an_edit_of_a_row_deleted_underneath_says_it_is_gone() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        let stale = db.get_automation(id).expect("get").expect("row");
        assert!(db.delete_automation(id).expect("delete"));
        assert!(!db
            .update_automation_definition(&stale, None)
            .expect("write"));
        let error = edit_outcome(&db, id).expect_err("nothing was written");
        assert!(error.contains("no longer exists"), "{error}");
    }

    /// An edit that moves the timing writes it only if the row still holds
    /// the timing it read, so a disable that landed meanwhile is not undone.
    #[test]
    fn an_edit_moving_the_timing_refuses_a_row_changed_underneath() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        let mut stale = db.get_automation(id).expect("get").expect("row");
        let read = (stale.enabled, stale.next_run_at);
        automation(&db, id, Some(false), false, false).expect("a disable lands");
        stale.schedule = AutomationSchedule::Cron {
            expr: "0 18 * * *".into(),
        };
        stale.next_run_at = Some(NOW + 5);
        assert!(!db
            .update_automation_definition(&stale, Some(read))
            .expect("write"));
        assert!(!only(&db).enabled, "the disable stands");
    }

    /// Enabling what is already enabled changes nothing: a fire that is due,
    /// or that run-now just marked, must not be pushed to the next slot.
    #[test]
    fn enabling_an_enabled_automation_keeps_a_pending_fire() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        assert!(db.trigger_automation_now(id).expect("run now"));
        let due = only(&db).next_run_at;
        automation(&db, id, Some(true), false, false).expect("enable");
        assert_eq!(only(&db).next_run_at, due, "the run-now survives");
    }

    /// An edit that leaves the schedule alone leaves its next run alone, so
    /// a fire already due — a run-now, or a one-shot not yet ticked — still
    /// happens after a rename.
    #[test]
    fn an_edit_that_keeps_the_schedule_keeps_a_due_fire() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        assert!(db.trigger_automation_now(id).expect("run now"));
        let due = only(&db).next_run_at;
        let rename = AutomationDraft {
            name: Some("renamed".into()),
            ..AutomationDraft::default()
        };
        save_automation(&db, Some(id), &rename, NOW + 120_000).expect("edit");
        assert_eq!(only(&db).next_run_at, due);

        let once = db
            .create_automation(&NewAutomation {
                name: "once".into(),
                enabled: true,
                schedule: AutomationSchedule::Once { at: NOW },
                timezone: None,
                action: AutomationAction::Exec {
                    command: "true".into(),
                },
                prompt: String::new(),
                next_run_at: Some(NOW),
                key: None,
                run: Default::default(),
            })
            .expect("insert");
        save_automation(&db, Some(once), &rename, NOW + 1_000).expect("a due one-shot renames");
    }

    /// The heartbeat claims a due row by compare-and-set on `next_run_at`.
    /// An edit that read the row before that claim and leaves the
    /// schedule alone must not write the old fire back, or the row is due
    /// again and fires twice — a one-shot re-enabled, a cron a second time.
    #[test]
    fn an_edit_racing_a_fire_does_not_re_arm_it() {
        let db = Database::open_in_memory().expect("db");
        let cron = stored_multi_repo(&db);
        let once = db
            .create_automation(&NewAutomation {
                name: "once".into(),
                enabled: true,
                schedule: AutomationSchedule::Once { at: NOW },
                timezone: None,
                action: AutomationAction::Exec {
                    command: "true".into(),
                },
                prompt: String::new(),
                next_run_at: Some(NOW),
                key: None,
                run: Default::default(),
            })
            .expect("insert");
        for (id, after_fire) in [(cron, Some(NOW + 3_600_000)), (once, None)] {
            let mut stale = db.get_automation(id).expect("get").expect("row");
            let due = stale.next_run_at.expect("due");
            assert!(db
                .claim_due_automation(id, due, after_fire, NOW)
                .expect("claim"));
            stale.name = "renamed".into();
            db.update_automation_definition(&stale, None)
                .expect("write");
            let row = db.get_automation(id).expect("get").expect("row");
            assert_eq!(row.name, "renamed");
            assert_eq!(
                row.next_run_at, after_fire,
                "#{id}: the fire is not re-armed"
            );
            assert_eq!(
                row.enabled,
                after_fire.is_some(),
                "#{id}: a spent one-shot stays off"
            );
        }
    }

    #[test]
    fn an_edit_refuses_what_it_cannot_apply() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        let cases: Vec<(AutomationDraft, &str)> = vec![
            (
                AutomationDraft {
                    time: Some("18:00".into()),
                    ..AutomationDraft::default()
                },
                "--trigger",
            ),
            (
                AutomationDraft {
                    weekday: Some(5),
                    ..AutomationDraft::default()
                },
                "--trigger",
            ),
            (
                AutomationDraft {
                    repo: Some("/srv/other".into()),
                    ..AutomationDraft::default()
                },
                "target",
            ),
            (
                AutomationDraft {
                    session: Some("6f1c1d8e-2a2b-4c1e-9d1f-0a1b2c3d4e5f".into()),
                    ..AutomationDraft::default()
                },
                "target",
            ),
        ];
        for (draft, expected) in cases {
            let error = save_automation(&db, Some(id), &draft, NOW)
                .expect_err(&format!("{draft:?} must be refused"));
            assert!(error.contains(expected), "{draft:?}: {error}");
        }
    }

    #[test]
    fn a_weekday_out_of_range_is_refused_not_read_as_monday() {
        let db = Database::open_in_memory().expect("db");
        for weekday in [-1, 8] {
            let draft = AutomationDraft {
                trigger: Some("weekly".into()),
                weekday: Some(weekday),
                ..spawn_draft()
            };
            let error = save_automation(&db, None, &draft, NOW).expect_err("refused");
            assert!(error.contains("weekday"), "{weekday}: {error}");
        }
    }

    /// Disabling clears the next run, so enabling has to compute it again —
    /// before this, an automation switched back on from a pane was enabled
    /// and never fired.
    #[test]
    fn enabling_from_a_pane_schedules_the_next_run_again() {
        let db = Database::open_in_memory().expect("db");
        let id = stored_multi_repo(&db);
        automation(&db, id, Some(false), false, false).expect("disable");
        assert_eq!(only(&db).next_run_at, None);
        automation(&db, id, Some(true), false, false).expect("enable");
        let auto = only(&db);
        assert!(auto.enabled);
        assert!(auto.next_run_at.is_some(), "an enabled schedule must fire");
    }

    #[test]
    fn enabling_a_spent_one_shot_is_refused_rather_than_silent() {
        let db = Database::open_in_memory().expect("db");
        let id = db
            .create_automation(&NewAutomation {
                name: "once".into(),
                enabled: false,
                schedule: AutomationSchedule::Once { at: 5 },
                timezone: None,
                action: AutomationAction::Exec {
                    command: "true".into(),
                },
                prompt: String::new(),
                next_run_at: None,
                key: None,
                run: Default::default(),
            })
            .expect("insert");
        let error = automation(&db, id, Some(true), false, false).expect_err("spent");
        assert!(error.contains("never fires"), "{error}");
        assert!(!only(&db).enabled);
    }

    #[test]
    fn a_key_of_digits_only_is_refused() {
        let db = Database::open_in_memory().expect("db");
        let draft = AutomationDraft {
            name: Some("sync".into()),
            trigger: Some("hourly".into()),
            command: Some("true".into()),
            ..AutomationDraft::default()
        };
        let error = create(&db, &draft, Some("7"), OnExisting::Fail, NOW).unwrap_err();
        assert!(error.to_string().contains("digits only"), "{error}");
        create(&db, &draft, Some("v7"), OnExisting::Fail, NOW).expect("a key with a letter");
    }

    /// Repeating a keyed create after its one-shot has come due keeps or
    /// rewrites the row rather than refusing a schedule that no longer fires:
    /// `keep` touches nothing, and an unchanged `update` keeps its pending fire.
    #[test]
    fn a_repeated_keyed_one_shot_create_keeps_or_updates_after_its_time() {
        let db = Database::open_in_memory().expect("db");
        let draft = AutomationDraft {
            name: Some("once".into()),
            trigger: Some(format!("at:{}", NOW + 10)),
            command: Some("true".into()),
            ..AutomationDraft::default()
        };
        let id = create(&db, &draft, Some("once"), OnExisting::Fail, NOW)
            .expect("create")
            .id();
        let later = NOW + 20;
        assert_eq!(
            create(&db, &draft, Some("once"), OnExisting::Keep, later),
            Ok(Saved::Kept(id))
        );
        assert_eq!(
            create(&db, &draft, Some("once"), OnExisting::Update, later),
            Ok(Saved::Updated(id))
        );
        assert_eq!(
            only(&db).next_run_at,
            Some(NOW + 10),
            "the pending fire stays"
        );
        // A changed schedule still has to fire again.
        let moved = AutomationDraft {
            trigger: Some(format!("at:{}", NOW + 15)),
            ..draft
        };
        let error = create(&db, &moved, Some("once"), OnExisting::Update, later).unwrap_err();
        assert!(error.to_string().contains("never fires again"), "{error}");
    }
}
