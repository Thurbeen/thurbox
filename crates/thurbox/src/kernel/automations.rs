//! Firing automations (`config::automations`): the tick that fires what is
//! due, and a fire asked for by hand.
//!
//! A fire is claimed in the file before it runs and its outcome is written
//! after, each under the file lock, so the lock is never held while a run
//! takes its time. What a fire does to a session is a call of Fleet's command
//! line, as every session command is (ADR-41): a send is `fleet send`, a spawn
//! is an interactive Task applied with `fleet apply --create-only` and then a
//! `fleet send`. An exec runs a command on this machine and touches no session.

use std::path::Path;

/// How much of an exec's output a run keeps, from its end.
const TAIL_CHARS: usize = 4096;

/// Unix millis now: the clock every fire is scheduled against.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// How often the interface ticks while it runs: often enough for a minute's
/// cron field, rarely enough to cost nothing.
const TICK_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// Tick on a thread of its own for as long as the process runs: the
/// interface's firer. A fire never runs on the render thread.
pub fn keep_ticking(fleet: std::path::PathBuf) {
    std::thread::spawn(move || loop {
        if let Err(e) = tick(&fleet, now_ms()) {
            tracing::warn!("automation tick: {e}");
        }
        std::thread::sleep(TICK_EVERY);
    });
}

/// What one fire did: the automation, when it started, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub name: String,
    pub run: crate::config::automations::Run,
}

/// Fire every enabled automation whose next run is due at `now`, one after
/// another, with Fleet at `fleet`. Runs left marked running past their
/// deadline are recorded as timed out first.
pub fn tick(fleet: &Path, now: u64) -> Result<Vec<Fired>, String> {
    let claimed = crate::config::automations::update(|all| {
        let mut claimed = Vec::new();
        for automation in &mut all.automations {
            automation.expire_runs(now);
            if !automation.enabled || automation.next_run_at.is_none_or(|at| at > now) {
                continue;
            }
            automation.start_run(now);
            automation.next_run_at = automation.next_after(now);
            claimed.push(automation.clone());
        }
        Ok(claimed)
    })?;
    Ok(claimed
        .into_iter()
        .map(|automation| finish(fleet, &automation, now))
        .collect())
}

/// Fire the automation `name` now, whatever its schedule says. A disabled one
/// is refused rather than switched on.
pub fn run_now(fleet: &Path, name: &str, now: u64) -> Result<Fired, String> {
    let automation = crate::config::automations::update(|all| {
        let automation = all
            .automations
            .iter_mut()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("no automation {name}"))?;
        if !automation.enabled {
            return Err(format!("{name} is disabled; enable it first"));
        }
        automation.start_run(now);
        Ok(automation.clone())
    })?;
    Ok(finish(fleet, &automation, now))
}

/// Run a claimed fire and record how it went.
fn finish(
    fleet: &Path,
    automation: &crate::config::automations::Automation,
    started_at: u64,
) -> Fired {
    let mut run = fire(fleet, automation);
    run.started_at = started_at;
    run.finished_at = Some(now_ms());
    let recorded = run.clone();
    let stored = crate::config::automations::update(|all| {
        if let Some(slot) = all
            .automations
            .iter_mut()
            .find(|a| a.name == automation.name)
            .and_then(|a| a.runs.iter_mut().find(|r| r.started_at == started_at))
        {
            *slot = recorded;
        }
        Ok(())
    });
    if let Err(e) = stored {
        tracing::warn!("could not record the run of {}: {e}", automation.name);
    }
    Fired {
        name: automation.name.clone(),
        run,
    }
}

/// What a fire did; its start and end are set by the caller.
fn fire(
    fleet: &Path,
    automation: &crate::config::automations::Automation,
) -> crate::config::automations::Run {
    let mut run = crate::config::automations::Run {
        started_at: 0,
        status: crate::config::automations::RunStatus::Success,
        detail: String::new(),
        finished_at: None,
        exit_code: None,
        session: None,
    };
    let done = match &automation.action {
        crate::config::automations::Action::Send { session } => {
            run.session = Some(session.clone());
            fleet_run(fleet, &["send", session, &automation.prompt], None)
                .map(|()| format!("sent to {session}"))
        }
        crate::config::automations::Action::Spawn { repo, agent } => {
            spawn(fleet, automation, repo, agent.as_deref()).map(|task| {
                let detail = format!("created {task}");
                run.session = Some(task);
                detail
            })
        }
        crate::config::automations::Action::Exec { command, cwd, env } => {
            return exec(automation, command, cwd.as_deref(), env, run);
        }
    };
    match done {
        Ok(detail) => run.detail = detail,
        Err(error) => {
            run.status = crate::config::automations::RunStatus::Error;
            run.detail = error;
        }
    }
    run
}

/// Create the session a spawn names and queue the prompt for it. Its Task is
/// named after the automation and the second it fired, so each fire makes a
/// session of its own.
fn spawn(
    fleet: &Path,
    automation: &crate::config::automations::Automation,
    repo: &Path,
    agent: Option<&str>,
) -> Result<String, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = format!("{}-{stamp}", automation.name);
    let task_name = super::command::task_name(&name);
    let mut spec = serde_json::json!({
        "host": "local",
        "workspace": {"dir": repo},
        "dispatch": true,
    });
    if let Some(agent) = agent {
        spec["harness"] = serde_json::json!(agent);
    }
    let task = serde_json::json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": "Task",
        "metadata": {
            "name": task_name,
            "labels": {crate::kernel::snapshot::fleet::NAME_LABEL: name},
        },
        "spec": spec,
    });
    fleet_run(
        fleet,
        &["apply", "--create-only", "-f", "-"],
        Some(&task.to_string()),
    )?;
    if !automation.prompt.is_empty() {
        fleet_run(fleet, &["send", &task_name, &automation.prompt], None)?;
    }
    Ok(task_name)
}

/// Run an exec to its exit or its deadline: in its directory or the home
/// directory, with the firer's environment, its own variables, and its name
/// as `THURBOX_AUTOMATION_NAME`.
fn exec(
    automation: &crate::config::automations::Automation,
    command: &str,
    cwd: Option<&Path>,
    env: &std::collections::BTreeMap<String, String>,
    mut run: crate::config::automations::Run,
) -> crate::config::automations::Run {
    let dir = cwd
        .map(Path::to_path_buf)
        .or_else(crate::paths::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut process = super::runs::command_for(command, &dir, None);
    process
        .envs(env)
        .env("THURBOX_AUTOMATION_NAME", &automation.name);
    let timeout = automation.timeout();
    match super::runs::capture(process, timeout) {
        super::runs::Run::Done(output) => {
            let (stdout, stderr) = (tail(&output.stdout), tail(&output.stderr));
            let said = [stdout, stderr]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            run.exit_code = output.status;
            let head = match (output.timed_out, output.status) {
                (true, _) => {
                    run.status = crate::config::automations::RunStatus::TimedOut;
                    format!("timed out after {}s", timeout.as_secs())
                }
                (false, Some(0)) => {
                    run.detail = if said.is_empty() { "ok".into() } else { said };
                    return run;
                }
                (false, Some(code)) => {
                    run.status = crate::config::automations::RunStatus::Error;
                    format!("exit {code}")
                }
                (false, None) => {
                    run.status = crate::config::automations::RunStatus::Error;
                    "exit signal".into()
                }
            };
            run.detail = match said.is_empty() {
                true => head,
                false => format!("{head}: {said}"),
            };
        }
        super::runs::Run::Failed(error) => {
            run.status = crate::config::automations::RunStatus::Error;
            run.detail = error;
        }
        super::runs::Run::Pending => {
            run.status = crate::config::automations::RunStatus::Error;
            run.detail = "it did not run".into();
        }
    }
    run
}

/// The trailing [`TAIL_CHARS`] of a stream, trimmed.
fn tail(text: &str) -> String {
    let text = text.trim();
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(TAIL_CHARS))
        .collect()
}

/// Run `fleet <args>` with `input` on its stdin; its error when it fails.
fn fleet_run(fleet: &Path, args: &[&str], input: Option<&str>) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(fleet)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start fleet {}: {e}", args[0]))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| format!("fleet {}: {e}", args[0]))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("fleet {}: {e}", args[0]))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let why = stderr
        .lines()
        .find_map(|line| line.strip_prefix("error: "))
        .unwrap_or(stderr.trim());
    Err(format!("fleet {}: {why}", args[0]))
}
