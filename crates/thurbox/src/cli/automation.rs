//! `thurbox-cli automation`: create, list, change, remove and fire the
//! automations in `automations.json` (`config::automations`).
//!
//! The interface ticks while it runs. `automation tick` is the same tick for
//! an OS timer (cron, a systemd timer), so a schedule keeps firing with no
//! interface open.

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::session::automation::{check_timezone, parse_trigger};

use super::output::CommandOutput;
use super::{CommandError, EXIT_USAGE};

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Create an automation: a trigger and exactly one of --session, --repo
    /// and --command.
    Create(CreateArgs),
    /// List the automations.
    List,
    /// Show one automation, its schedule and its recent runs.
    Show { name: String },
    /// Change an automation's schedule, prompt, command or state. Its action
    /// stays; create a new one to change it.
    Edit(EditArgs),
    /// Remove an automation and its runs.
    Remove { name: String },
    /// Fire an enabled automation now and answer with the run.
    Run { name: String },
    /// Fire every automation that is due (for an OS timer).
    Tick,
}

#[derive(Args, Debug)]
pub struct Schedule {
    /// When to fire: `hourly` | `daily` | `weekdays` | `weekly` |
    /// `cron:"<expr>"` | `at:<unix_millis>`.
    #[arg(long)]
    pub trigger: Option<String>,
    /// Time of day `HH:MM` for a preset (default `00:00`).
    #[arg(long)]
    pub time: Option<String>,
    /// Day of week (0=Sun..6=Sat, or 7=Sun) for `weekly` (default Mon).
    #[arg(long)]
    pub weekday: Option<u32>,
    /// IANA timezone (e.g. `Europe/Zurich`); default system local.
    #[arg(long)]
    pub timezone: Option<String>,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    /// Unique among automations.
    #[arg(long)]
    pub name: String,
    #[command(flatten)]
    pub schedule: Schedule,
    /// What a send or spawn types into its session.
    #[arg(long, default_value = "")]
    pub prompt: String,
    /// Send: the session (Fleet Task) to queue the prompt for.
    #[arg(long, conflicts_with_all = ["repo", "command"])]
    pub session: Option<String>,
    /// Spawn: the repository a new session works in.
    #[arg(long, conflicts_with = "command")]
    pub repo: Option<String>,
    /// Spawn: the agent (a Fleet harness); default Fleet's.
    #[arg(long, requires = "repo")]
    pub agent: Option<String>,
    /// Exec: a command to run headlessly on this machine.
    #[arg(long)]
    pub command: Option<String>,
    /// Exec: the directory it runs in (default: your home directory).
    #[arg(long, requires = "command")]
    pub cwd: Option<String>,
    /// Exec: `KEY=VALUE` to run with (repeatable).
    #[arg(long, requires = "command")]
    pub env: Vec<String>,
    /// Seconds a run may take (default 600). An exec is killed at it.
    #[arg(long)]
    pub timeout_secs: Option<u64>,
    /// Create it disabled.
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Args, Debug)]
pub struct EditArgs {
    pub name: String,
    #[command(flatten)]
    pub schedule: Schedule,
    #[arg(long)]
    pub prompt: Option<String>,
    /// Exec: the new command.
    #[arg(long)]
    pub command: Option<String>,
    #[arg(long)]
    pub timeout_secs: Option<u64>,
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,
    #[arg(long)]
    pub disable: bool,
}

pub fn run(
    action: Action,
    fleet: &crate::fleet_link::Fleets,
) -> Result<CommandOutput, CommandError> {
    let now = crate::kernel::automations::now_ms();
    match action {
        Action::Create(args) => create(args, now),
        Action::List => list(),
        Action::Show { name } => show(&name),
        Action::Edit(args) => edit(args, now),
        Action::Remove { name } => remove(&name),
        Action::Run { name } => fire(fleet, &name, now),
        Action::Tick => tick(fleet, now),
    }
}

/// The schedule `schedule` spells, as the stored trigger, after checking it
/// and its timezone.
fn trigger_of(schedule: &Schedule) -> Result<Option<String>, CommandError> {
    let usage = |e: String| CommandError::with_code(e, EXIT_USAGE);
    if let Some(timezone) = &schedule.timezone {
        check_timezone(timezone).map_err(usage)?;
    }
    let Some(trigger) = &schedule.trigger else {
        if schedule.time.is_some() || schedule.weekday.is_some() {
            return Err(usage(
                "--time and --weekday say when within a --trigger preset; give the trigger too"
                    .into(),
            ));
        }
        return Ok(None);
    };
    let parsed =
        parse_trigger(trigger, schedule.time.as_deref(), schedule.weekday).map_err(usage)?;
    parsed.check().map_err(usage)?;
    Ok(Some(parsed.trigger()))
}

fn create(args: CreateArgs, now: u64) -> Result<CommandOutput, CommandError> {
    let usage = |e: &str| CommandError::with_code(e, EXIT_USAGE);
    let trigger = trigger_of(&args.schedule)?.ok_or_else(|| usage("--trigger is required"))?;
    let action = match (args.session, args.repo, args.command) {
        (Some(session), None, None) => crate::config::automations::Action::Send { session },
        (None, Some(repo), None) => crate::config::automations::Action::Spawn {
            repo: crate::paths::expand_tilde(&repo),
            agent: args.agent,
        },
        (None, None, Some(command)) => crate::config::automations::Action::Exec {
            command,
            cwd: args.cwd.as_deref().map(crate::paths::expand_tilde),
            env: parse_env(&args.env)?,
        },
        _ => return Err(usage("name exactly one of --session, --repo and --command")),
    };
    let mut automation = crate::config::automations::Automation {
        name: args.name.clone(),
        enabled: !args.disabled,
        trigger,
        timezone: args.schedule.timezone,
        prompt: args.prompt,
        action,
        timeout_secs: args.timeout_secs,
        created_at: now,
        last_run_at: None,
        next_run_at: None,
        runs: Vec::new(),
    };
    automation.next_run_at = automation.next_after(now);
    if automation.enabled && automation.next_run_at.is_none() {
        return Err(usage("the trigger has no future occurrence"));
    }
    let created = automation.clone();
    crate::config::automations::update(move |all| {
        if all.automations.iter().any(|a| a.name == created.name) {
            return Err(format!("an automation named {} exists", created.name));
        }
        all.automations.push(created);
        Ok(())
    })?;
    Ok(record(
        &automation,
        format!("Created automation {}.", automation.name),
    ))
}

fn parse_env(pairs: &[String]) -> Result<std::collections::BTreeMap<String, String>, CommandError> {
    pairs
        .iter()
        .map(|pair| {
            pair.split_once('=')
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| {
                    CommandError::with_code(format!("--env {pair}: expected KEY=VALUE"), EXIT_USAGE)
                })
        })
        .collect()
}

fn list() -> Result<CommandOutput, CommandError> {
    let all = crate::config::automations::load()?;
    let rows: Vec<Value> = all.automations.iter().map(summary).collect();
    let human = match rows.is_empty() {
        true => "No automations.".to_string(),
        false => all
            .automations
            .iter()
            .map(|a| {
                format!(
                    "{}  {}  {}  {}",
                    a.name,
                    if a.enabled { "on" } else { "off" },
                    a.trigger,
                    a.action.kind()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    Ok(CommandOutput::new(json!(rows), human))
}

fn find(name: &str) -> Result<crate::config::automations::Automation, CommandError> {
    crate::config::automations::load()?
        .automations
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| CommandError::from(format!("no automation {name}")))
}

fn show(name: &str) -> Result<CommandOutput, CommandError> {
    let automation = find(name)?;
    Ok(record(
        &automation,
        serde_json::to_string_pretty(&automation).unwrap_or_default(),
    ))
}

fn edit(args: EditArgs, now: u64) -> Result<CommandOutput, CommandError> {
    let trigger = trigger_of(&args.schedule)?;
    let name = args.name.clone();
    let rescheduled =
        trigger.is_some() || args.schedule.timezone.is_some() || args.enable || args.disable;
    let edited = crate::config::automations::update(move |all| {
        let automation = all
            .automations
            .iter_mut()
            .find(|a| a.name == args.name)
            .ok_or_else(|| format!("no automation {}", args.name))?;
        if let Some(trigger) = trigger {
            automation.trigger = trigger;
        }
        if let Some(timezone) = args.schedule.timezone {
            automation.timezone = Some(timezone).filter(|tz| !tz.is_empty());
        }
        if let Some(prompt) = args.prompt {
            automation.prompt = prompt;
        }
        if let Some(command) = args.command {
            match &mut automation.action {
                crate::config::automations::Action::Exec { command: held, .. } => *held = command,
                _ => return Err("--command changes an exec automation only".into()),
            }
        }
        if args.timeout_secs.is_some() {
            automation.timeout_secs = args.timeout_secs;
        }
        if args.enable || args.disable {
            automation.enabled = args.enable;
        }
        // Only a change of when it fires moves the next run: an edit of what it
        // does must not cancel a run that is due, or skip one.
        if rescheduled {
            automation.next_run_at = automation.next_after(now);
        }
        Ok(automation.clone())
    })?;
    Ok(record(&edited, format!("Updated automation {name}.")))
}

fn remove(name: &str) -> Result<CommandOutput, CommandError> {
    crate::config::automations::update(|all| {
        let before = all.automations.len();
        all.automations.retain(|a| a.name != name);
        match all.automations.len() < before {
            true => Ok(()),
            false => Err(format!("no automation {name}")),
        }
    })?;
    Ok(CommandOutput::new(
        json!({ "removed": name }),
        format!("Removed automation {name}."),
    ))
}

fn fire(
    fleet: &crate::fleet_link::Fleets,
    name: &str,
    now: u64,
) -> Result<CommandOutput, CommandError> {
    let fired = crate::kernel::automations::run_now(fleet, name, now)?;
    let mut out = run_output(&fired);
    if fired.run.status != crate::config::automations::RunStatus::Success {
        out.failure = Some(format!("{name}: {}", fired.run.detail));
    }
    Ok(out)
}

fn tick(fleet: &crate::fleet_link::Fleets, now: u64) -> Result<CommandOutput, CommandError> {
    let fired = crate::kernel::automations::tick(fleet, now)?;
    let rows: Vec<Value> = fired
        .iter()
        .map(|f| json!({ "name": f.name, "run": f.run }))
        .collect();
    let human = match fired.is_empty() {
        true => "Nothing was due.".to_string(),
        false => fired
            .iter()
            .map(|f| format!("{}: {}", f.name, f.run.detail))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    Ok(CommandOutput::new(json!({ "fired": rows }), human))
}

fn run_output(fired: &crate::kernel::automations::Fired) -> CommandOutput {
    CommandOutput::new(
        json!({ "name": fired.name, "run": fired.run }),
        format!("{}: {}", fired.name, fired.run.detail),
    )
}

/// The fields a list shows of one automation.
fn summary(a: &crate::config::automations::Automation) -> Value {
    json!({
        "name": a.name,
        "enabled": a.enabled,
        "trigger": a.trigger,
        "action": a.action.kind(),
        "next_run_at": a.next_run_at,
        "last_outcome": a.runs.last().map(|r| r.status),
    })
}

fn record(a: &crate::config::automations::Automation, human: String) -> CommandOutput {
    CommandOutput::new(serde_json::to_value(a).unwrap_or(Value::Null), human)
}
