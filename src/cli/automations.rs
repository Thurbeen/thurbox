//! Automation CRUD subcommands for `thurbox-cli`.
//!
//! Automations are persisted to the shared database; the running TUI's tick
//! loop is what actually fires them. `run` just marks an automation due so the
//! TUI picks it up on its next tick.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::action::{self, SpawnDeliverError};
use crate::cli::output::{self, CommandOutput};
use crate::session::automation::parse_trigger;
use crate::session::{Automation, AutomationAction, AutomationRun, AutomationRunStatus, SessionId};
use crate::session_ops::SpawnRequest;
use crate::storage::automations::NewAutomation;
use crate::storage::Database;
use crate::sync::current_time_millis;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Create an automation.
    Create {
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// When to fire: `hourly` | `daily` | `weekdays` | `weekly` |
        /// `cron:"<expr>"` | `at:<unix_millis>`.
        #[arg(long)]
        trigger: String,
        /// Time of day `HH:MM` for presets (default `00:00`).
        #[arg(long)]
        time: Option<String>,
        /// Day of week (0=Sun..6=Sat, or 7=Sun) for the `weekly` preset
        /// (default Mon).
        #[arg(long)]
        weekday: Option<u32>,
        /// IANA timezone (e.g. `Europe/Zurich`); default system local.
        #[arg(long)]
        timezone: Option<String>,
        /// Prompt text sent on fire (send/spawn actions). Unused by `--command`.
        #[arg(long, default_value = "")]
        prompt: String,
        /// Send action: target an existing session by UUID.
        #[arg(long)]
        session: Option<String>,
        /// Spawn action: repository path to run a new session in.
        #[arg(long)]
        repo: Option<String>,
        /// Spawn action: optional worktree branch (created if missing).
        #[arg(long)]
        worktree: Option<String>,
        /// Spawn action: base branch for a new worktree (default `main`).
        #[arg(long)]
        base: Option<String>,
        /// Agent name (spawn action; default registry agent).
        #[arg(long)]
        agent: Option<String>,
        /// Exec action: shell command to run headlessly on fire (no session,
        /// no agent). Mutually exclusive with --session/--repo.
        #[arg(long)]
        command: Option<String>,
        /// Create the automation disabled.
        #[arg(long)]
        disabled: bool,
    },
    /// List all automations.
    List,
    /// Show one automation by id.
    Show {
        /// Automation id.
        id: i64,
    },
    /// Edit an automation.
    Edit {
        /// Automation id.
        id: i64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        trigger: Option<String>,
        #[arg(long)]
        time: Option<String>,
        #[arg(long)]
        weekday: Option<u32>,
        #[arg(long)]
        timezone: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        /// Enable the automation.
        #[arg(long)]
        enabled: bool,
        /// Disable the automation.
        #[arg(long)]
        disabled: bool,
    },
    /// Remove an automation.
    Remove {
        /// Automation id.
        id: i64,
    },
    /// Fire an automation now (marks it due for the running TUI).
    Run {
        /// Automation id.
        id: i64,
    },
    /// Show an automation's run history.
    Runs {
        /// Automation id.
        id: i64,
        /// Maximum entries (default 20).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Fire all currently-due automations headlessly (no TUI required). This is
    /// the entry point the tmux heartbeat keeper and any systemd/cron timer call.
    Tick,
}

pub fn run(action: Action, db: &Database) -> Result<CommandOutput, String> {
    match action {
        Action::Create {
            name,
            trigger,
            time,
            weekday,
            timezone,
            prompt,
            session,
            repo,
            worktree,
            base,
            agent,
            command,
            disabled,
        } => create_automation(
            db, name, trigger, time, weekday, timezone, prompt, session, repo, worktree, base,
            agent, command, disabled,
        ),
        Action::List => list_automations(db),
        Action::Show { id } => {
            let auto = load(db, id)?;
            Ok(CommandOutput::new(
                automation_to_json(&auto),
                render_automation_detail(&auto),
            ))
        }
        Action::Edit {
            id,
            name,
            trigger,
            time,
            weekday,
            timezone,
            prompt,
            enabled,
            disabled,
        } => edit_automation(
            db, id, name, trigger, time, weekday, timezone, prompt, enabled, disabled,
        ),
        Action::Remove { id } => remove_automation(db, id),
        Action::Run { id } => trigger_automation(db, id),
        Action::Runs { id, limit } => {
            let runs = db
                .list_automation_runs(id, limit.unwrap_or(20))
                .map_err(|e| format!("list_automation_runs: {e}"))?;
            let json = Value::Array(runs.iter().map(run_to_json).collect());
            Ok(CommandOutput::new(json, render_run_history(id, &runs)))
        }
        Action::Tick => {
            let json = tick(db)?;
            let human = render_tick(&json);
            Ok(CommandOutput::new(json, human))
        }
    }
}

/// Handle `automation create`: validate, persist, and arm the heartbeat.
#[allow(clippy::too_many_arguments)]
fn create_automation(
    db: &Database,
    name: String,
    trigger: String,
    time: Option<String>,
    weekday: Option<u32>,
    timezone: Option<String>,
    prompt: String,
    session: Option<String>,
    repo: Option<String>,
    worktree: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    command: Option<String>,
    disabled: bool,
) -> Result<CommandOutput, String> {
    // An exec automation carries the command, not a prompt; send/spawn need one.
    if command.is_none() && prompt.trim().is_empty() {
        return Err("prompt must not be empty".into());
    }
    let schedule = parse_trigger(&trigger, time.as_deref(), weekday)?;
    let action = resolve_action(
        session,
        repo,
        worktree,
        base,
        agent,
        Vec::new(),
        command,
        db,
    )?;
    let next_run_at = if disabled {
        None
    } else {
        schedule.next_after(current_time_millis(), timezone.as_deref())
    };
    let new = NewAutomation {
        name,
        enabled: !disabled,
        schedule,
        timezone,
        action,
        prompt,
        next_run_at,
    };
    let id = db
        .create_automation(&new)
        .map_err(|e| format!("create_automation: {e}"))?;
    if !disabled {
        arm_heartbeat();
    }
    let auto = db
        .get_automation(id)
        .map_err(|e| format!("get_automation: {e}"))?
        .ok_or("automation vanished after insert")?;
    let human = format!(
        "Created automation #{} '{}' ({}){}",
        auto.id,
        auto.name,
        auto.schedule.kind(),
        if auto.enabled { "" } else { " — disabled" }
    );
    Ok(CommandOutput::new(automation_to_json(&auto), human))
}

/// Handle `automation list`.
fn list_automations(db: &Database) -> Result<CommandOutput, String> {
    let autos = db
        .list_automations()
        .map_err(|e| format!("list_automations: {e}"))?;
    let json = Value::Array(autos.iter().map(automation_to_json).collect());
    Ok(CommandOutput::new(json, render_automation_list(&autos)))
}

/// Handle `automation edit`: apply the supplied field overrides and persist.
#[allow(clippy::too_many_arguments)]
fn edit_automation(
    db: &Database,
    id: i64,
    name: Option<String>,
    trigger: Option<String>,
    time: Option<String>,
    weekday: Option<u32>,
    timezone: Option<String>,
    prompt: Option<String>,
    enabled: bool,
    disabled: bool,
) -> Result<CommandOutput, String> {
    if enabled && disabled {
        return Err("--enabled and --disabled are mutually exclusive".into());
    }
    let mut auto = load(db, id)?;
    apply_edit_overrides(&mut auto, name, trigger, time, weekday, timezone, prompt)?;
    if disabled {
        auto.enabled = false;
    }
    if enabled {
        auto.enabled = true;
    }
    // Recompute next fire after any schedule/timezone/enabled change.
    auto.next_run_at = if auto.enabled {
        auto.schedule
            .next_after(current_time_millis(), auto.timezone.as_deref())
    } else {
        None
    };
    db.update_automation(&auto)
        .map_err(|e| format!("update_automation: {e}"))?;
    if auto.enabled {
        arm_heartbeat();
    }
    let auto = load(db, id)?;
    Ok(CommandOutput::new(
        automation_to_json(&auto),
        render_automation_detail(&auto),
    ))
}

/// Apply the name/prompt/timezone/schedule overrides supplied to `edit`.
///
/// `--time`/`--weekday` only shape a preset trigger, so they require `--trigger`
/// in the same call (the stored schedule is a raw cron expression with no
/// recoverable preset to re-apply them to). Supplying them alone is a clear
/// error rather than a silent no-op.
fn apply_edit_overrides(
    auto: &mut Automation,
    name: Option<String>,
    trigger: Option<String>,
    time: Option<String>,
    weekday: Option<u32>,
    timezone: Option<String>,
    prompt: Option<String>,
) -> Result<(), String> {
    if let Some(n) = name {
        auto.name = n;
    }
    if let Some(p) = prompt {
        if p.trim().is_empty() {
            return Err("prompt must not be empty".into());
        }
        auto.prompt = p;
    }
    if let Some(tz) = timezone {
        auto.timezone = if tz.is_empty() { None } else { Some(tz) };
    }
    if let Some(t) = trigger {
        auto.schedule = parse_trigger(&t, time.as_deref(), weekday)?;
    } else if time.is_some() || weekday.is_some() {
        return Err("--time/--weekday only apply with --trigger (a preset)".into());
    }
    Ok(())
}

/// Handle `automation remove`.
fn remove_automation(db: &Database, id: i64) -> Result<CommandOutput, String> {
    match db.delete_automation(id) {
        Ok(true) => Ok(CommandOutput::new(
            json!({ "removed": true, "id": id }),
            format!("Removed automation #{id}."),
        )),
        Ok(false) => Err(format!("Automation not found: {id}")),
        Err(e) => Err(format!("delete_automation: {e}")),
    }
}

/// Handle `automation run`: mark the automation due for the next tick.
fn trigger_automation(db: &Database, id: i64) -> Result<CommandOutput, String> {
    match db.trigger_automation_now(id) {
        Ok(true) => Ok(CommandOutput::new(
            json!({ "triggered": true, "id": id }),
            format!("Triggered automation #{id} (fires on the next tick)."),
        )),
        Ok(false) => Err(format!("Automation not found: {id}")),
        Err(e) => Err(format!("trigger_automation_now: {e}")),
    }
}

/// Render the automation list as an aligned table (or a friendly empty line).
fn render_automation_list(autos: &[Automation]) -> String {
    if autos.is_empty() {
        return "No automations.".to_string();
    }
    let rows: Vec<Vec<String>> = autos
        .iter()
        .map(|a| {
            vec![
                a.id.to_string(),
                if a.enabled { "on" } else { "off" }.to_string(),
                a.name.clone(),
                a.schedule.kind().to_string(),
                action::action_label(Some(&a.action)),
            ]
        })
        .collect();
    output::table(&["ID", "STATE", "NAME", "SCHEDULE", "ACTION"], &rows)
}

/// Render a single automation as an aligned key/value block.
fn render_automation_detail(a: &Automation) -> String {
    let pairs: Vec<(&str, String)> = vec![
        ("id", a.id.to_string()),
        ("name", a.name.clone()),
        ("enabled", a.enabled.to_string()),
        (
            "schedule",
            format!("{} ({})", a.schedule.kind(), a.schedule.spec()),
        ),
        ("timezone", output::dash(a.timezone.as_deref())),
        ("action", action::action_label(Some(&a.action))),
        ("prompt", a.prompt.clone()),
    ];
    output::kv(&pairs)
}

/// Render an automation's run history as a table.
fn render_run_history(id: i64, runs: &[AutomationRun]) -> String {
    if runs.is_empty() {
        return format!("No run history for automation #{id}.");
    }
    let rows: Vec<Vec<String>> = runs
        .iter()
        .map(|r| {
            vec![
                r.id.to_string(),
                r.status.as_str().to_string(),
                r.detail.clone(),
            ]
        })
        .collect();
    output::table(&["RUN", "STATUS", "DETAIL"], &rows)
}

/// One-line human summary of an `automation tick`.
fn render_tick(v: &Value) -> String {
    let count = |key: &str| v.get(key).and_then(Value::as_array).map_or(0, Vec::len);
    let fired = count("fired");
    let skipped = count("skipped");
    let healed = count("healed");
    format!("Tick: {fired} fired, {skipped} skipped, {healed} extension(s) healed.")
}

/// Fire every due automation headlessly: claim (atomic CAS, so this is safe to
/// run alongside the TUI and other tickers), perform the action, record the run.
fn tick(db: &Database) -> Result<Value, String> {
    // Self-heal active extensions before firing: this runs from the tmux
    // heartbeat keeper every 60s, so a deleted flow session/automation is
    // recreated even with the TUI closed. Best-effort — heal messages are
    // reported but never abort the due-automation pass below.
    let healed = crate::session_ops::heal_active_extensions(db);
    for m in &healed {
        tracing::info!("{m}");
    }
    // Keep the auto-activated built-in extensions wired up headlessly too.
    for m in &crate::session_ops::ensure_builtin_extensions(db) {
        tracing::info!("{m}");
    }
    // Best-effort retention sweep of the inter-session mailbox (read messages
    // older than the default window), so the queue self-bounds with the TUI
    // closed. Never abort the due-automation pass over it.
    if let Err(e) = db.prune_old_messages() {
        tracing::debug!("prune_old_messages: {e}");
    }
    let now = current_time_millis();
    let due = db
        .due_automations(now)
        .map_err(|e| format!("due_automations: {e}"))?;
    let mut fired = Vec::new();
    let mut skipped = Vec::new();
    for auto in due {
        let next = auto.schedule.next_after(now, auto.timezone.as_deref());
        let claimed = db
            .claim_due_automation(auto.id, auto.next_run_at.unwrap_or(0), next, now)
            .map_err(|e| format!("claim_due_automation: {e}"))?;
        if !claimed {
            // Another firer (TUI / concurrent tick) won the claim. This logs at
            // debug! (invisible at the default level), so report it in the JSON too.
            tracing::debug!(
                automation_id = auto.id,
                "automation claim lost to a concurrent firer"
            );
            skipped.push(json!({ "id": auto.id, "reason": "claim-lost" }));
            continue;
        }
        let (status, detail, related) = fire_headless(db, &auto);
        let _ = db.record_automation_run(auto.id, status, &detail, related);
        fired.push(json!({
            "id": auto.id,
            "status": status.as_str(),
            "detail": detail,
        }));
    }
    // Headless remote-status poll: the live control-mode channels
    // (subscription / psmux poller) die with the TUI, so this keeps remote
    // sessions' hook states flowing at the heartbeat's 60 s cadence — the TUI
    // stays the sub-second channel while open. AFTER the due-automation pass:
    // an unreachable host costs up to ConnectTimeout per attempt, which must
    // never delay a scheduled firing. Skipped when the built-in hooks
    // extension is opted out (nothing sets the pane option then).
    if crate::session_ops::hooks_enabled(db) {
        let polled = crate::session_ops::remote_hooks::poll_remote_hook_states(db);
        if polled > 0 {
            tracing::info!("remote status poll: {polled} hook state(s) updated");
        }
    }
    Ok(json!({ "fired": fired, "skipped": skipped, "healed": healed }))
}

/// Execute one automation's action without a TUI, returning the run outcome.
///
/// `send` types into the still-alive tmux window; `spawn` creates a session
/// headlessly (the TUI adopts it by name on next startup) and delivers the
/// prompt via a deferred tmux timer once the agent boots. Local-tmux scoped —
/// a future remote backend would branch here.
fn fire_headless(
    db: &Database,
    auto: &Automation,
) -> (AutomationRunStatus, String, Option<SessionId>) {
    // tmux helpers are reached via fully-qualified paths (no `use crate::agent`)
    // to keep the cli module free of an `agent` import — see
    // tests/architecture_rules.rs::cli_module_isolation.
    match &auto.action {
        AutomationAction::Send { session_id } => fire_send(db, auto, *session_id),
        AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            extra_repos,
        } => fire_spawn(
            db,
            auto,
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            extra_repos,
        ),
        AutomationAction::Exec { command } => fire_exec(command),
    }
}

/// Execute an `exec` automation headlessly via the shared runner. No session is
/// involved (deterministic scheduled job).
fn fire_exec(command: &str) -> (AutomationRunStatus, String, Option<SessionId>) {
    let (status, detail) = crate::session_ops::run_exec_command(command);
    (status, detail, None)
}

/// Execute a `send` automation: type the prompt into the target session's
/// still-alive tmux window.
fn fire_send(
    db: &Database,
    auto: &Automation,
    session_id: SessionId,
) -> (AutomationRunStatus, String, Option<SessionId>) {
    let name = match db.get_session_name(session_id) {
        Ok(Some(name)) => name,
        Ok(None) => {
            return (
                AutomationRunStatus::Skipped,
                "target session not found".into(),
                None,
            )
        }
        Err(e) => {
            return (
                AutomationRunStatus::Error,
                format!("get_session_name: {e}"),
                None,
            )
        }
    };
    if !crate::agent::tmux::window_exists(&name) {
        return (
            AutomationRunStatus::Skipped,
            "target session not running".into(),
            None,
        );
    }
    match crate::agent::tmux::send_prompt_now(&name, &auto.prompt) {
        Ok(()) => (
            AutomationRunStatus::Success,
            format!("sent to {session_id}"),
            Some(session_id),
        ),
        Err(e) => (AutomationRunStatus::Error, e.to_string(), None),
    }
}

/// Execute a `spawn` automation: reuse an existing window or spawn a new
/// headless session, then deliver the prompt once the agent boots.
#[allow(clippy::too_many_arguments)]
fn fire_spawn(
    db: &Database,
    auto: &Automation,
    repo_path: &std::path::Path,
    worktree_branch: &Option<String>,
    base_branch: &Option<String>,
    agent: &Option<String>,
    extra_repos: &[crate::session::ExtraRepo],
) -> (AutomationRunStatus, String, Option<SessionId>) {
    let name = format!("auto-{}", auto.id);
    // Reuse an existing session window (later fires / restored sessions).
    if crate::agent::tmux::window_exists(&name) {
        // The reused window's session id has no cheap lookup here.
        return match crate::agent::tmux::send_prompt_now(&name, &auto.prompt) {
            Ok(()) => (AutomationRunStatus::Success, format!("reused {name}"), None),
            Err(e) => (AutomationRunStatus::Error, e.to_string(), None),
        };
    }
    let req = SpawnRequest {
        name: name.clone(),
        repo_path: repo_path.to_path_buf(),
        worktree_branch: worktree_branch.clone(),
        base_branch: base_branch.clone(),
        agent: agent.clone(),
        extra_repos: extra_repos.to_vec(),
        ..Default::default()
    };
    match action::spawn_and_deliver(db, &name, req, &auto.prompt) {
        Ok(session_id) => (
            AutomationRunStatus::Success,
            format!("spawned {name}"),
            Some(session_id),
        ),
        // A spawned-but-undelivered run still records its session id.
        Err(SpawnDeliverError::Deliver {
            session_id,
            message,
        }) => (AutomationRunStatus::Error, message, Some(session_id)),
        Err(SpawnDeliverError::Spawn(e)) => (AutomationRunStatus::Error, e, None),
    }
}

fn load(db: &Database, id: i64) -> Result<Automation, String> {
    db.get_automation(id)
        .map_err(|e| format!("get_automation: {e}"))?
        .ok_or_else(|| format!("Automation not found: {id}"))
}

/// Resolve the action from the shared flags; an automation, unlike a task,
/// must carry one.
#[allow(clippy::too_many_arguments)]
fn resolve_action(
    session: Option<String>,
    repo: Option<String>,
    worktree: Option<String>,
    base: Option<String>,
    agent: Option<String>,
    extra_repos: Vec<crate::session::ExtraRepo>,
    command: Option<String>,
    db: &Database,
) -> Result<AutomationAction, String> {
    action::resolve_action(
        action::ActionFlags {
            session,
            repo,
            worktree,
            base,
            agent,
            extra_repos,
            command,
        },
        db,
    )?
    .ok_or_else(|| "specify --session (send), --repo (spawn), or --command (exec)".into())
}

fn automation_to_json(a: &Automation) -> Value {
    let action = action::action_to_json(Some(&a.action));
    json!({
        "id": a.id,
        "name": a.name,
        "enabled": a.enabled,
        "schedule": { "kind": a.schedule.kind(), "spec": a.schedule.spec() },
        "timezone": a.timezone,
        "action": action,
        "prompt": a.prompt,
        "created_at": a.created_at,
        "updated_at": a.updated_at,
        "last_run_at": a.last_run_at,
        "next_run_at": a.next_run_at,
    })
}

/// Best-effort: ensure the tmux heartbeat keeper is running so the automation
/// fires even when no TUI is attached. Failures (e.g. tmux missing) are
/// non-fatal — the automation still works while the TUI is up.
///
/// Gated on `[features] automations`: when disabled the TUI neither fires
/// schedules nor arms the heartbeat, so the CLI must not arm it either (it
/// would spawn a keeper window that can never fire anything).
pub(crate) fn arm_heartbeat() {
    if !crate::session::settings::global().features.automations {
        return;
    }
    let cli = crate::agent::tmux::resolve_cli_binary();
    if let Err(e) = crate::agent::tmux::ensure_automation_heartbeat(&cli) {
        eprintln!("warning: failed to arm automation heartbeat: {e}");
    }
}

fn run_to_json(r: &AutomationRun) -> Value {
    json!({
        "id": r.id,
        "automation_id": r.automation_id,
        "started_at": r.started_at,
        "status": r.status.as_str(),
        "detail": r.detail,
        "related_session_id": r.related_session_id.map(|id| id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_reports_fired_and_skipped_arrays() {
        let db = Database::open_in_memory().unwrap();
        let v = tick(&db).unwrap();
        assert_eq!(v["fired"], json!([]));
        assert_eq!(v["skipped"], json!([]));
    }

    #[test]
    fn render_tick_counts_fired_skipped_and_healed() {
        let v = json!({
            "fired": [{ "id": 1 }, { "id": 2 }],
            "skipped": [{ "id": 3 }],
            "healed": [],
        });
        assert_eq!(
            render_tick(&v),
            "Tick: 2 fired, 1 skipped, 0 extension(s) healed."
        );
    }

    #[test]
    fn render_automation_list_empty_is_friendly() {
        assert_eq!(render_automation_list(&[]), "No automations.");
    }

    #[test]
    fn action_label_distinguishes_send_and_spawn() {
        assert_eq!(
            action::action_label(Some(&AutomationAction::Send {
                session_id: SessionId::default(),
            })),
            "send"
        );
        assert_eq!(
            action::action_label(Some(&AutomationAction::Spawn {
                repo_path: "/x".into(),
                worktree_branch: None,
                base_branch: None,
                agent: None,
                extra_repos: Vec::new(),
            })),
            "spawn"
        );
    }

    fn sample_automation() -> Automation {
        Automation {
            id: 1,
            name: "noop".into(),
            enabled: true,
            schedule: crate::session::AutomationSchedule::Cron {
                expr: "0 9 * * *".into(),
            },
            timezone: None,
            action: AutomationAction::Send {
                session_id: SessionId::default(),
            },
            prompt: "hi".into(),
            created_at: 0,
            updated_at: 0,
            last_run_at: None,
            next_run_at: None,
        }
    }

    #[test]
    fn edit_time_or_weekday_without_trigger_errors() {
        let mut auto = sample_automation();
        let err = apply_edit_overrides(
            &mut auto,
            None,
            None,
            Some("09:30".into()),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("--trigger"), "got {err}");

        let mut auto = sample_automation();
        let err =
            apply_edit_overrides(&mut auto, None, None, None, Some(3), None, None).unwrap_err();
        assert!(err.contains("--trigger"), "got {err}");
    }

    #[test]
    fn edit_time_with_trigger_applies() {
        let mut auto = sample_automation();
        apply_edit_overrides(
            &mut auto,
            None,
            Some("daily".into()),
            Some("06:15".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(auto.schedule.spec(), "15 6 * * *");
    }

    #[test]
    fn edit_rejects_blank_prompt() {
        let mut auto = sample_automation();
        let err = apply_edit_overrides(&mut auto, None, None, None, None, None, Some("   ".into()))
            .unwrap_err();
        assert!(err.contains("prompt"), "got {err}");
    }

    #[test]
    fn create_rejects_blank_prompt() {
        let db = Database::open_in_memory().unwrap();
        let err = create_automation(
            &db,
            "n".into(),
            "daily".into(),
            None,
            None,
            None,
            "   ".into(),
            None,
            Some("/repo".into()),
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap_err();
        assert!(err.contains("prompt"), "got {err}");
    }

    #[test]
    fn resolve_action_spawn_carries_extra_repos() {
        let db = Database::open_in_memory().unwrap();
        let extra = super::super::parse_extra_repos(&["/b@main".into()], &["/c".into()]);
        let action =
            resolve_action(None, Some("/a".into()), None, None, None, extra, None, &db).unwrap();
        match action {
            AutomationAction::Spawn { extra_repos, .. } => {
                assert_eq!(extra_repos.len(), 2);
                assert!(extra_repos[0].worktree);
                assert_eq!(extra_repos[0].base_branch.as_deref(), Some("main"));
                assert!(!extra_repos[1].worktree);
            }
            other => panic!("expected spawn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_action_command_builds_exec() {
        let db = Database::open_in_memory().unwrap();
        let action = resolve_action(
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
            Some("~/sync.sh".into()),
            &db,
        )
        .unwrap();
        assert!(matches!(action, AutomationAction::Exec { command } if command == "~/sync.sh"));
    }

    #[test]
    fn resolve_action_rejects_command_with_session() {
        let db = Database::open_in_memory().unwrap();
        let err = resolve_action(
            Some("s".into()),
            None,
            None,
            None,
            None,
            Vec::new(),
            Some("cmd".into()),
            &db,
        )
        .unwrap_err();
        assert!(err.contains("only one"), "got {err}");
    }

    #[test]
    fn run_to_json_emits_related_session_id() {
        let sid = SessionId::default();
        let run = AutomationRun {
            id: 1,
            automation_id: 2,
            started_at: 3,
            status: AutomationRunStatus::Success,
            detail: "sent".into(),
            related_session_id: Some(sid),
        };
        assert_eq!(run_to_json(&run)["related_session_id"], sid.to_string());

        let run = AutomationRun {
            related_session_id: None,
            ..run
        };
        assert_eq!(run_to_json(&run)["related_session_id"], Value::Null);
    }
}
