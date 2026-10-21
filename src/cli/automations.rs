//! Automation subcommands for `thurbox-cli`.
//!
//! Argument parsing and output only: every verb is
//! [`crate::session_ops::automations`]'s, the same code the interface's pane
//! writes through (issue #1468). Automations fire from `automation tick` — the
//! heartbeat keeper's, an OS timer's or a hand-run one — and `run --wait`
//! fires one on the spot.

use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::cli::action;
use crate::cli::notify;
use crate::cli::output::{self, CommandOutput};
use crate::cli::{CommandError, EXIT_AMBIGUOUS, EXIT_CONFLICT, EXIT_GONE, EXIT_TIMEOUT};
use crate::session::{Automation, AutomationRun, AutomationRunStatus};
use crate::session_ops::automations::{self as ops, Draft, RefError, RunNowError, SaveError};
use crate::storage::Database;
use crate::sync::current_time_millis;

/// What `create --key` does when the key is already taken.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OnExisting {
    /// Rewrite it in place, keeping its id and run history.
    Update,
    /// Leave it alone and answer with it.
    Keep,
    /// Refuse, exiting 5.
    #[default]
    Fail,
}

impl From<OnExisting> for ops::OnExisting {
    fn from(value: OnExisting) -> Self {
        match value {
            OnExisting::Update => ops::OnExisting::Update,
            OnExisting::Keep => ops::OnExisting::Keep,
            OnExisting::Fail => ops::OnExisting::Fail,
        }
    }
}

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
        #[arg(long)]
        prompt: Option<String>,
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
        /// An address for the automation, unique among them: with it, running
        /// the same create twice is safe (see --on-existing).
        #[arg(long)]
        key: Option<String>,
        /// What to do when --key is already taken.
        #[arg(long = "on-existing", value_enum, default_value_t = OnExisting::Fail, requires = "key")]
        on_existing: OnExisting,
        /// Seconds a run may take (default 600). An exec is killed at it; a run
        /// of any kind still going past it is recorded as timed out.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        /// Exec action: directory to run in (default: your home directory).
        #[arg(long)]
        cwd: Option<String>,
        /// Exec action: `KEY=VALUE` to run with (repeatable).
        #[arg(long)]
        env: Vec<String>,
    },
    /// List automations.
    List {
        /// Only the automation with this key.
        #[arg(long)]
        key: Option<String>,
        /// Only automations with this name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Show one automation by id, key or unique name.
    #[command(alias = "get")]
    Show {
        /// Automation id, key or name.
        reference: String,
    },
    /// Edit an automation. Its target stays; create a new one to change it.
    Edit {
        /// Automation id, key or name.
        reference: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        trigger: Option<String>,
        #[arg(long)]
        time: Option<String>,
        #[arg(long)]
        weekday: Option<u32>,
        /// IANA timezone; `""` goes back to system local.
        #[arg(long)]
        timezone: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        /// Exec action: the new command. Keeps the id and run history.
        #[arg(long)]
        command: Option<String>,
        /// Seconds a run may take.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        /// Exec action: directory to run in; `""` goes back to home.
        #[arg(long)]
        cwd: Option<String>,
        /// Exec action: `KEY=VALUE` to run with (repeatable); replaces the
        /// whole set.
        #[arg(long, conflicts_with = "clear_env")]
        env: Vec<String>,
        /// Exec action: run with no variables of its own.
        #[arg(long)]
        clear_env: bool,
        /// Enable the automation.
        #[arg(long, conflicts_with = "disabled")]
        enabled: bool,
        /// Disable the automation.
        #[arg(long)]
        disabled: bool,
    },
    /// Remove an automation and its run history.
    #[command(alias = "delete")]
    Remove {
        /// Automation id, key or name.
        reference: String,
    },
    /// Fire an enabled automation on the next tick (marks it due), or with
    /// --wait now, answering with the run. A disabled one is refused rather
    /// than switched on.
    ///
    /// --wait exits 0 when the run succeeded, 1 when it failed, 4 when it
    /// timed out, 5 when a run of it is already in flight and 6 when the
    /// automation was deleted before or while it ran.
    Run {
        /// Automation id, key or name.
        reference: String,
        /// Fire it now, on this process, and answer with its run. Runs nothing
        /// else: no other due automation and none of the tick's housekeeping.
        #[arg(long)]
        wait: bool,
        /// With --wait: seconds this run may take, instead of its own timeout.
        #[arg(long, requires = "wait", value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        /// With --wait: run a disabled automation once, leaving it disabled.
        #[arg(long, requires = "wait")]
        force: bool,
    },
    /// Show an automation's run history.
    Runs {
        /// Automation id, key or name.
        reference: String,
        /// Maximum entries (default 20).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Fire all currently-due automations headlessly (no TUI required). This is
    /// the entry point the heartbeat and any systemd/cron timer call.
    Tick,
}

pub fn run(
    action: Action,
    db: &Database,
    backends: &super::Backends<'_>,
) -> Result<CommandOutput, CommandError> {
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
            key,
            on_existing,
            timeout,
            cwd,
            env,
        } => {
            let draft = Draft {
                name: Some(name),
                trigger: Some(trigger),
                time,
                weekday: weekday.map(i64::from),
                timezone,
                prompt,
                enabled: Some(!disabled),
                session,
                repo,
                branch: worktree,
                base,
                agent,
                command,
                timeout_secs: timeout,
                cwd,
                env: env_flags(&env)?,
            };
            create(db, backends.get(), &draft, key.as_deref(), on_existing)
        }
        Action::List { key, name } => list(db, key.as_deref(), name.as_deref()),
        Action::Show { reference } => {
            let auto = resolve(db, &reference)?;
            Ok(CommandOutput::new(
                automation_to_json(&auto),
                render_automation_detail(&auto),
            ))
        }
        Action::Edit {
            reference,
            name,
            trigger,
            time,
            weekday,
            timezone,
            prompt,
            command,
            timeout,
            cwd,
            env,
            clear_env,
            enabled,
            disabled,
        } => {
            let id = resolve(db, &reference)?.id;
            let draft = Draft {
                name,
                trigger,
                time,
                weekday: weekday.map(i64::from),
                timezone,
                prompt,
                enabled: match (enabled, disabled) {
                    (true, _) => Some(true),
                    (_, true) => Some(false),
                    _ => None,
                },
                command,
                timeout_secs: timeout,
                cwd,
                env: if clear_env {
                    Some(Vec::new())
                } else {
                    env_flags(&env)?
                },
                ..Draft::default()
            };
            ops::edit(db, id, &draft, current_time_millis())?;
            let auto = load(db, id)?;
            if auto.enabled {
                arm_heartbeat(backends.get());
            }
            Ok(CommandOutput::new(
                automation_to_json(&auto),
                render_automation_detail(&auto),
            ))
        }
        Action::Remove { reference } => {
            let id = resolve(db, &reference)?.id;
            match ops::remove(db, id)? {
                true => Ok(CommandOutput::new(
                    json!({ "removed": true, "id": id }),
                    format!("Removed automation #{id}."),
                )),
                false => Err(format!("Automation not found: {id}").into()),
            }
        }
        Action::Run {
            reference,
            wait: false,
            ..
        } => {
            let id = resolve(db, &reference)?.id;
            ops::trigger(db, id)?;
            Ok(CommandOutput::new(
                json!({ "triggered": true, "id": id }),
                format!("Triggered automation #{id} (fires on the next tick)."),
            ))
        }
        Action::Run {
            reference,
            wait: true,
            timeout,
            force,
        } => {
            let id = resolve(db, &reference)?.id;
            run_wait(
                db,
                backends.get(),
                id,
                force,
                timeout.map(std::time::Duration::from_secs),
            )
        }
        Action::Runs { reference, limit } => {
            let id = resolve(db, &reference)?.id;
            let runs = db
                .list_automation_runs(id, limit.unwrap_or(20))
                .map_err(|e| format!("list_automation_runs: {e}"))?;
            let json = Value::Array(runs.iter().map(run_to_json).collect());
            Ok(CommandOutput::new(json, render_run_history(id, &runs)))
        }
        Action::Tick => {
            let json = match tick(db, backends.get()) {
                Ok(json) => json,
                Err(e) => {
                    ops::tick_log::record(&ops::tick_log::failed(&e));
                    return Err(e.into());
                }
            };
            ops::tick_log::record(&last_tick(&json));
            let human = render_tick(&json);
            Ok(CommandOutput::new(json, human))
        }
    }
}

/// What a tick that ran records for `runtime status`: its counts, and every
/// automation it could not fire with why — the heartbeat discards the JSON
/// these are in.
fn last_tick(json: &Value) -> ops::tick_log::LastTick {
    let errors = json["skipped"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["reason"] == "error")
        .map(|s| {
            format!(
                "automation #{}: {}",
                s["id"],
                s["detail"].as_str().unwrap_or("no detail")
            )
        })
        .collect();
    ops::tick_log::LastTick {
        at: current_time_millis(),
        ok: true,
        error: None,
        fired: json["fired"].as_array().map_or(0, Vec::len),
        failed: json["failed"].as_u64().unwrap_or(0) as usize,
        errors,
    }
}

/// `--env KEY=VALUE` flags as the draft takes them: `None` when none were
/// passed, so an edit without them leaves the set alone.
fn env_flags(tokens: &[String]) -> Result<Option<Vec<(String, String)>>, String> {
    if tokens.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        super::sessions::parse_env(tokens)?.into_iter().collect(),
    ))
}

/// The automation `reference` names, with the exit code a driver branches on
/// when it names several.
fn resolve(db: &Database, reference: &str) -> Result<Automation, CommandError> {
    ops::resolve(db, reference).map_err(|e| match e {
        RefError::Ambiguous(m) => CommandError::with_code(m, EXIT_AMBIGUOUS),
        RefError::NotFound(m) | RefError::Storage(m) => CommandError::from(m),
    })
}

/// Handle `automation create`: save, arm the heartbeat, answer with the row and
/// what happened to it.
fn create(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    draft: &Draft,
    key: Option<&str>,
    on_existing: OnExisting,
) -> Result<CommandOutput, CommandError> {
    let saved = ops::create(db, draft, key, on_existing.into(), current_time_millis()).map_err(
        |e| match e {
            SaveError::Conflict(m) => CommandError::with_code(m, EXIT_CONFLICT),
            SaveError::Invalid(m) => CommandError::from(m),
        },
    )?;
    let auto = load(db, saved.id())?;
    if auto.enabled {
        arm_heartbeat(backends);
    }
    let verb = match saved {
        ops::Saved::Created(_) => "Created",
        ops::Saved::Updated(_) => "Updated",
        ops::Saved::Kept(_) => "Kept",
    };
    let human = format!(
        "{verb} automation #{} '{}' ({}){}",
        auto.id,
        auto.name,
        auto.schedule.kind(),
        if auto.enabled { "" } else { " — disabled" }
    );
    let mut json = automation_to_json(&auto);
    json["outcome"] = json!(saved.as_str());
    Ok(CommandOutput::new(json, human))
}

/// Handle `automation list`.
fn list(
    db: &Database,
    key: Option<&str>,
    name: Option<&str>,
) -> Result<CommandOutput, CommandError> {
    let autos: Vec<Automation> = db
        .list_automations()
        .map_err(|e| format!("list_automations: {e}"))?
        .into_iter()
        .filter(|a| key.map_or(true, |k| a.key.as_deref() == Some(k)))
        .filter(|a| name.map_or(true, |n| a.name == n))
        .collect();
    let json = Value::Array(autos.iter().map(automation_to_json).collect());
    Ok(CommandOutput::new(json, render_automation_list(&autos))
        .list("automations", &["id", "name", "enabled", "next_run_at"])
        .empty("0 automations scheduled")
        .help([
            "thurbox-cli automation show <ref>   its schedule, action and prompt",
            "thurbox-cli automation run <ref> --wait   fire it now and see the run",
            "thurbox-cli automation tick   run everything that is due now",
        ]))
}

/// Handle `automation run --wait`: fire it here and answer with the run, the
/// exit code saying how it went.
fn run_wait(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    id: i64,
    force: bool,
    timeout: Option<std::time::Duration>,
) -> Result<CommandOutput, CommandError> {
    let run = ops::run_now(db, backends, id, force, timeout).map_err(|e| match e {
        RunNowError::InFlight(m) => CommandError::with_code(m, EXIT_CONFLICT),
        RunNowError::Gone(m) => CommandError::with_code(m, EXIT_GONE),
        RunNowError::Disabled(m) | RunNowError::Storage(m) => CommandError::from(m),
    })?;
    let json = run_to_json(&run);
    let human = format!(
        "Run #{} of automation #{id}: {} — {}",
        run.id,
        run.status.as_str(),
        run.detail
    );
    Ok(match run.status {
        AutomationRunStatus::Success => CommandOutput::new(json, human),
        AutomationRunStatus::TimedOut => {
            CommandOutput::failed(json, human, format!("run #{} timed out", run.id))
                .exiting_with(EXIT_TIMEOUT)
        }
        _ => CommandOutput::failed(
            json,
            human,
            format!("run #{} {}: {}", run.id, run.status.as_str(), run.detail),
        ),
    })
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
    let mut pairs: Vec<(&str, String)> = vec![
        ("id", a.id.to_string()),
        ("name", a.name.clone()),
        ("key", output::dash(a.key.as_deref())),
        ("enabled", a.enabled.to_string()),
        (
            "schedule",
            format!("{} ({})", a.schedule.kind(), a.schedule.spec()),
        ),
        ("timezone", output::dash(a.timezone.as_deref())),
        ("action", action::action_label(Some(&a.action))),
        ("prompt", a.prompt.clone()),
        ("timeout", format!("{}s", a.run.timeout().as_secs())),
    ];
    if let Some(cwd) = &a.run.cwd {
        pairs.push(("cwd", cwd.display().to_string()));
    }
    for (k, v) in &a.run.env {
        pairs.push(("env", format!("{k}={v}")));
    }
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
                r.exit_code
                    .map_or_else(|| "-".to_string(), |c| c.to_string()),
                r.duration_ms()
                    .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms")),
                r.detail.clone(),
            ]
        })
        .collect();
    output::table(&["RUN", "STATUS", "EXIT", "TOOK", "DETAIL"], &rows)
}

/// One-line human summary of an `automation tick`.
fn render_tick(v: &Value) -> String {
    let count = |key: &str| v.get(key).and_then(Value::as_array).map_or(0, Vec::len);
    let fired = count("fired");
    let skipped = count("skipped");
    let healed = count("healed");
    format!("Tick: {fired} fired, {skipped} skipped, {healed} extension(s) healed.")
}

/// Fire every due automation headlessly — claimed by compare-and-set, so this
/// is safe beside other tickers — and run the housekeeping a headless machine
/// needs while the due execs run.
fn tick(db: &Database, backends: &crate::backend::BackendRegistry) -> Result<Value, String> {
    // Self-heal active extensions before firing: this runs from the
    // heartbeat every 60s, so an extension's deleted session/automation is
    // recreated even with the TUI closed. Best-effort — heal messages are
    // reported but never abort the due-automation pass below.
    let healed = crate::session_ops::heal_active_extensions(db, backends);
    for m in &healed {
        tracing::info!("{m}");
    }
    // Keep the auto-activated built-in extensions wired up headlessly too.
    for m in &crate::session_ops::ensure_builtin_extensions(db, backends) {
        tracing::info!("{m}");
    }
    // Best-effort retention sweep of the inter-session mailbox (read messages
    // older than the default window), so the queue self-bounds with the TUI
    // closed. Never abort the due-automation pass over it.
    if let Err(e) = db.prune_old_messages() {
        tracing::debug!("prune_old_messages: {e}");
    }
    let (pass, housekeeping) = ops::tick(db, backends, current_time_millis(), || {
        housekeeping(db, backends)
    })?;
    let fired: Vec<Value> = pass
        .fired
        .iter()
        .map(|f| {
            let mut entry = json!({
                "id": f.automation_id,
                "run_id": f.run_id,
                "status": f.status.as_str(),
                "detail": f.detail,
            });
            if let Some(error) = &f.history_error {
                entry["history_error"] = Value::String(error.clone());
            }
            entry
        })
        .collect();
    let alerts = failure_alerts(db, &pass);
    let skipped: Vec<Value> = pass
        .skipped
        .iter()
        .map(|s| json!({ "id": s.automation_id, "reason": s.reason, "detail": s.detail }))
        .collect();
    let mut out = json!({
        "fired": fired,
        "failed": pass.failed(),
        "skipped": skipped,
        "timed_out": pass.timed_out,
        "alerts": alerts,
        "healed": healed,
    });
    if let (Value::Object(out), Value::Object(more)) = (&mut out, housekeeping) {
        out.extend(more);
    }
    Ok(out)
}

/// How long a failing automation's alert holds off the next one while it keeps
/// failing. A success ends the streak sooner.
const FAILURE_ALERT_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// The opt-in failing-run alert (issue #1508): one desktop notification per
/// failing streak of an automation, sent the way `notify send` sends one and
/// deduplicated by the same lease. A success forgets the streak, so the next
/// failure alerts again.
fn failure_alerts(db: &Database, pass: &ops::DuePass) -> Vec<Value> {
    if !crate::session::settings::global()
        .notifications
        .on_automation_failure
    {
        return Vec::new();
    }
    let mut alerts = Vec::new();
    for fired in &pass.fired {
        let key = format!("automation-failure:{}", fired.automation_id);
        match fired.status {
            AutomationRunStatus::Success => {
                if let Err(e) = notify::forget(db, &key) {
                    tracing::debug!("forget {key}: {e}");
                }
            }
            AutomationRunStatus::Error | AutomationRunStatus::TimedOut => {
                let name = db
                    .get_automation(fired.automation_id)
                    .ok()
                    .flatten()
                    .map_or_else(|| format!("#{}", fired.automation_id), |a| a.name);
                let sent = notify::send(
                    db,
                    &notify::Message {
                        title: format!("Automation {name} failed"),
                        body: fired.detail.lines().next().unwrap_or_default().to_string(),
                        session: None,
                        dedupe: Some((key, FAILURE_ALERT_INTERVAL_SECS)),
                    },
                );
                let mut alert = sent.to_json();
                alert["id"] = json!(fired.automation_id);
                alerts.push(alert);
            }
            _ => {}
        }
    }
    alerts
}

/// What the tick does besides firing, run while the due execs run so a hung
/// one holds none of it up.
fn housekeeping(db: &Database, backends: &crate::backend::BackendRegistry) -> Value {
    // Headless status poll: the live channels ride an attached interface and
    // die with it, so this keeps every route's hook states flowing at the
    // heartbeat's cadence — each read from the backend that serves the route,
    // the interface staying the sub-second channel while open. After the
    // sends and spawns: an unreachable host costs up to ConnectTimeout per
    // attempt, which must never delay a scheduled firing. Skipped when the
    // built-in hooks extension is opted out (no hook reports anything then).
    if crate::session_ops::hooks_enabled(db) {
        let polled = crate::session_ops::remote_hooks::poll_hook_states(db, backends);
        if polled > 0 {
            tracing::info!("status poll: {polled} hook state(s) updated");
        }
    }
    // Shared hosts, mirrored at the heartbeat's cadence so `session list`
    // readers see peer sessions with the interface closed. After the
    // sends and spawns for the same reason as the remote poll above.
    let synced: Vec<Value> = match crate::session_ops::mirror::sync(db, None, false) {
        Ok(reports) => reports.iter().map(|r| r.to_json()).collect(),
        Err(e) => {
            tracing::warn!("mirror sync: {e}");
            Vec::new()
        }
    };
    // A soft delete asked for headlessly — from a peer, or from this CLI —
    // has no interface here to reap it once the undo window has closed.
    let reaped = crate::session_ops::reap_overdue_soft_deletes(db, backends);
    // And a force delete whose teardown never reached its host: this is the
    // pass that finishes it, so an agent orphaned by a machine that was down
    // for a minute is collected rather than left running there for good.
    // After the mirror above, which is what may just have taken the delete to
    // a host that came back on its own.
    let teardowns = crate::session_ops::retry_owed_remote_teardowns(db, backends);
    // Last: a collector that is down costs a request timeout, and nothing
    // above may wait on it.
    let telemetry = export_telemetry(db);
    json!({
        "synced": synced,
        "reaped": reaped,
        "teardowns": teardowns,
        "telemetry": telemetry,
    })
}

/// Export thurbox's own session metrics and session event logs (ADR-37),
/// reporting what went — `null` while `[telemetry]` exports nothing. A failure
/// is logged and reported, never returned: the rows wait for the next tick.
fn export_telemetry(db: &Database) -> Value {
    let settings = &crate::session::settings::global().telemetry;
    if !settings.exports_thurbox() {
        return Value::Null;
    }
    let shared_hosts: std::collections::HashSet<String> = crate::agent::host_config::load_all()
        .hosts
        .iter()
        .filter(|h| h.shareable())
        .map(|h| h.name.clone())
        .collect();
    let sessions = db.list_active_sessions().unwrap_or_default();
    let facts = super::sessions::SessionFacts::load(db);
    let lost = db.load_lost_sessions().unwrap_or_default();
    let registry = crate::agent::agent_config::load_or_seed();
    let counts: Vec<crate::telemetry::SessionCount> = sessions
        .iter()
        .map(|s| crate::telemetry::SessionCount {
            id: s.id,
            state: if lost.contains_key(&s.id) {
                crate::telemetry::LOST.to_string()
            } else {
                facts.assess(&registry, s, None).state_word().to_string()
            },
            agent: s.agent.clone(),
            backend: s.backend_type.clone(),
        })
        .collect();
    let identity = crate::telemetry::Identity {
        version: crate::agent::version_check::current_version().to_string(),
        instance: crate::backend::instance::local_socket_name(),
    };
    match crate::telemetry::export(db, settings, &identity, &counts, &shared_hosts) {
        Ok(report) => json!({
            "events": report.events,
            "cursor": report.cursor,
            "skipped": report.skipped,
        }),
        Err(e) => {
            tracing::warn!("telemetry export: {e}");
            json!({ "error": e })
        }
    }
}

fn load(db: &Database, id: i64) -> Result<Automation, String> {
    db.get_automation(id)
        .map_err(|e| format!("get_automation: {e}"))?
        .ok_or_else(|| format!("Automation not found: {id}"))
}

fn automation_to_json(a: &Automation) -> Value {
    let action = action::action_to_json(Some(&a.action));
    let env: serde_json::Map<String, Value> = a
        .run
        .env
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    json!({
        "id": a.id,
        "key": a.key,
        "name": a.name,
        "enabled": a.enabled,
        "schedule": { "kind": a.schedule.kind(), "spec": a.schedule.spec() },
        "timezone": a.timezone,
        "action": action,
        "prompt": a.prompt,
        "timeout_secs": a.run.timeout().as_secs(),
        "cwd": a.run.cwd.as_ref().map(|p| p.display().to_string()),
        "env": env,
        "created_at": a.created_at,
        "updated_at": a.updated_at,
        "last_run_at": a.last_run_at,
        "next_run_at": a.next_run_at,
    })
}

/// Best-effort: ask this machine's backend — the registry's default — to keep
/// the heartbeat running, so the automation fires with no interface attached.
/// The interface runs no scheduler, so without the keeper (or an OS timer) a
/// schedule fires only when somebody runs `automation tick`. A failure (its
/// multiplexer missing, say) is a warning, not an error: the write stands.
///
/// Gated on `[features] automations`, which turns heartbeat arming off for the
/// interface and the CLI alike.
pub(crate) fn arm_heartbeat(backends: &crate::backend::BackendRegistry) {
    if !crate::session::settings::global().features.automations {
        return;
    }
    if let Err(e) = crate::session_ops::arm_heartbeat(backends) {
        eprintln!("warning: failed to arm automation heartbeat: {e}");
    }
}

fn run_to_json(r: &AutomationRun) -> Value {
    json!({
        "id": r.id,
        "automation_id": r.automation_id,
        "started_at": r.started_at,
        "finished_at": r.finished_at,
        "duration_ms": r.duration_ms(),
        "status": r.status.as_str(),
        "exit_code": r.exit_code,
        "stdout_tail": r.stdout_tail,
        "stderr_tail": r.stderr_tail,
        "detail": r.detail,
        "related_session_id": r.related_session_id.map(|id| id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::automation::parse_trigger;
    use crate::session::{AutomationAction, SessionId};
    use crate::storage::automations::NewAutomation;

    fn opt_out_builtins(db: &Database) {
        // The tick self-heals the built-in extensions, whose outside-reaching
        // payloads resolve against `$HOME` — which `TestPathGuard` does not
        // redirect, because they target the *agent's* config dir rather than
        // thurbox's. Without opting out, running the test suite installs hook
        // files into the developer's own `~/.codex`, `~/.grok`, … Opt both
        // built-ins out: these tests are about what `tick` reports, not about
        // what an install does.
        for name in [
            crate::session_ops::builtin_hooks::HOOKS_EXTENSION_NAME,
            crate::session_ops::builtin_ui_skill::UI_SKILL_EXTENSION_NAME,
        ] {
            db.set_builtin_extension_optout(name, true).unwrap();
        }
    }

    #[test]
    fn tick_reports_fired_and_skipped_arrays() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        opt_out_builtins(&db);
        let v = tick(&db, &crate::backend::registry::inert()).unwrap();
        assert_eq!(v["fired"], json!([]));
        assert_eq!(v["skipped"], json!([]));
        // The mirror pass and the overdue reap report too, empty here: no
        // shareable host is configured and nothing was soft-deleted.
        assert_eq!(v["synced"], json!([]));
        assert_eq!(v["reaped"], json!([]));
    }

    /// A run-history write that fails is reported with the fire, so a row
    /// left `running` by a failed write is not mistaken for a hung action.
    #[test]
    fn tick_reports_a_run_history_write_that_failed() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        opt_out_builtins(&db);
        let id = db
            .create_automation(&NewAutomation {
                name: "probe".into(),
                enabled: true,
                schedule: parse_trigger("hourly", None, None).unwrap(),
                timezone: None,
                action: AutomationAction::Exec {
                    command: "echo ok".into(),
                },
                prompt: String::new(),
                next_run_at: Some(1),
                key: None,
                run: Default::default(),
            })
            .unwrap();
        db.conn_ref()
            .execute_batch(
                "CREATE TRIGGER refuse_finish BEFORE UPDATE ON automation_runs \
                 BEGIN SELECT RAISE(ABORT, 'history is read-only'); END;",
            )
            .unwrap();

        let v = tick(&db, &crate::backend::registry::inert()).unwrap();
        let fired = &v["fired"][0];
        assert_eq!(fired["id"], id);
        assert_eq!(fired["status"], "success", "{v}");
        let error = fired["history_error"].as_str().unwrap_or_default();
        assert!(error.contains("history is read-only"), "{v}");
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
    fn create_rejects_blank_prompt() {
        let db = Database::open_in_memory().unwrap();
        let draft = Draft {
            name: Some("n".into()),
            trigger: Some("daily".into()),
            prompt: Some("   ".into()),
            repo: Some("/repo".into()),
            ..Draft::default()
        };
        let err = create(
            &db,
            &crate::backend::registry::inert(),
            &draft,
            None,
            OnExisting::Fail,
        )
        .unwrap_err();
        assert!(err.contains("prompt"), "got {}", err.message);
    }

    #[test]
    fn run_to_json_emits_related_session_id_and_the_run_s_fields() {
        let sid = SessionId::default();
        let run = AutomationRun {
            id: 1,
            automation_id: 2,
            started_at: 3,
            status: AutomationRunStatus::Success,
            detail: "sent".into(),
            related_session_id: Some(sid),
            finished_at: Some(10),
            exit_code: None,
            stdout_tail: None,
            stderr_tail: None,
        };
        let json = run_to_json(&run);
        assert_eq!(json["related_session_id"], sid.to_string());
        assert_eq!(json["duration_ms"], 7);

        let run = AutomationRun {
            related_session_id: None,
            finished_at: None,
            ..run
        };
        let json = run_to_json(&run);
        assert_eq!(json["related_session_id"], Value::Null);
        assert_eq!(json["duration_ms"], Value::Null, "still running");
    }

    /// An automation the tick could not fire is in the tick's record, with
    /// why, so `runtime status` and the tick log show it.
    #[test]
    fn a_tick_that_could_not_fire_an_automation_records_it() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let db = Database::open_in_memory().unwrap();
        opt_out_builtins(&db);
        let id = db
            .create_automation(&NewAutomation {
                name: "refused".into(),
                enabled: true,
                schedule: parse_trigger("hourly", None, None).unwrap(),
                timezone: None,
                action: AutomationAction::Exec {
                    command: "true".into(),
                },
                prompt: String::new(),
                next_run_at: Some(1),
                key: None,
                run: Default::default(),
            })
            .unwrap();
        db.conn_ref()
            .execute_batch(
                "CREATE TRIGGER refuse_run BEFORE INSERT ON automation_runs \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let v = tick(&db, &crate::backend::registry::inert()).unwrap();
        let recorded = last_tick(&v);
        assert!(recorded.ok);
        assert_eq!(recorded.errors.len(), 1, "{v}");
        assert!(
            recorded.errors[0].starts_with(&format!("automation #{id}: "))
                && recorded.errors[0].contains("refused"),
            "{:?}",
            recorded.errors
        );
    }
}
