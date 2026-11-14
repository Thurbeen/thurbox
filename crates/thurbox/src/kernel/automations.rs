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

use crate::fleet_link::FleetLink;

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
    let fleet = local(fleet);
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
        .map(|automation| finish(&fleet, &automation, now))
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
    Ok(finish(&local(fleet), &automation, now))
}

/// The Fleet `program` runs on this machine.
fn local(program: &Path) -> FleetLink {
    FleetLink::local(program.to_string_lossy())
}

/// Run a claimed fire and record how it went.
fn finish(
    fleet: &FleetLink,
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
    fleet: &FleetLink,
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
            fleet
                .run(&["send", session, &automation.prompt], None)
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
    fleet: &FleetLink,
    automation: &crate::config::automations::Automation,
    repo: &Path,
    agent: Option<&str>,
) -> Result<String, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = format!("{}-{stamp}", automation.name);
    let task_name = super::command::task_name(&name);
    let mut spec = serde_json::json!({
        "host": "local",
        // The automation runs in the repository as it is, as a person's
        // session there would before it asked for a worktree.
        "workspace": [super::command::dir_url("file", repo)],
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
    fleet.run(
        &["apply", "--create-only", "-f", "-"],
        Some(&task.to_string()),
    )?;
    if !automation.prompt.is_empty() {
        fleet.run(&["send", &task_name, &automation.prompt], None)?;
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
    let mut process = super::runs::command_for(command, &dir);
    process
        .envs(env)
        .env("THURBOX_AUTOMATION_NAME", &automation.name);
    let timeout = automation.timeout();
    let ended = match bounded(process, timeout) {
        Ok(ended) => ended,
        Err(error) => {
            run.status = crate::config::automations::RunStatus::Error;
            run.detail = format!("could not start it: {error}");
            return run;
        }
    };
    let said = [ended.stdout, ended.stderr]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    run.exit_code = ended.code;
    let head = match (ended.timed_out, ended.code) {
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
    run
}

/// How an exec ended: its exit code (`None` when a signal or its deadline
/// ended it) and the end of each stream.
struct Ended {
    code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

/// Run `command` to its exit or `timeout`, whichever is first.
///
/// Its own runner rather than `runs::capture`, whose plugin programs keep the
/// head of their output and stop reading at the cap. An automation keeps the
/// tail, so each pipe is drained to its end and only the last
/// [`TAIL_CHARS`] kept: a command that prints a lot still runs to its end
/// instead of dying on a closed pipe. It runs as the leader of a process
/// group (in a job object on Windows), and the whole group is killed once it
/// exits or its deadline passes, so a child it left in the background does
/// not outlive the run, and no grandchild holds a pipe open past the bound.
fn bounded(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<Ended> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrap = process_wrap::std::CommandWrap::from(command);
    #[cfg(unix)]
    wrap.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    wrap.wrap(process_wrap::std::JobObject);
    let mut child = wrap.spawn()?;
    let stdout = drain(child.stdout().take());
    let stderr = drain(child.stderr().take());
    let deadline = std::time::Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.code()),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => break Err(()),
            Err(_) => break Ok(None),
        }
    };
    // Whatever is left in the group goes with it.
    let _ = child.start_kill();
    let _ = child.wait();
    // The pipes close once the group is gone; a reader still waiting on a
    // child that escaped the group is left behind after a short grace.
    let grace = std::time::Instant::now() + std::time::Duration::from_millis(500);
    Ok(Ended {
        timed_out: code.is_err(),
        code: code.unwrap_or(None),
        stdout: stdout.take_by(grace),
        stderr: stderr.take_by(grace),
    })
}

/// One pipe drained to its end on a thread of its own, keeping the tail.
struct Drained {
    kept: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
    done: std::sync::mpsc::Receiver<()>,
}

/// Bytes kept from the end of a stream: enough for [`TAIL_CHARS`] characters
/// of up to four bytes each.
const TAIL_BYTES: usize = TAIL_CHARS * 4;

fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> Drained {
    let kept = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let (finished, done) = std::sync::mpsc::channel();
    let into = std::sync::Arc::clone(&kept);
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 64 * 1024];
            while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                if let Ok(mut kept) = into.lock() {
                    kept.extend(&chunk[..n]);
                    let excess = kept.len().saturating_sub(TAIL_BYTES);
                    kept.drain(..excess);
                }
            }
        }
        let _ = finished.send(());
    });
    Drained { kept, done }
}

impl Drained {
    /// The tail kept, once the pipe ended or `by` passed.
    fn take_by(self, by: std::time::Instant) -> String {
        let _ = self
            .done
            .recv_timeout(by.saturating_duration_since(std::time::Instant::now()));
        let bytes: Vec<u8> = self
            .kept
            .lock()
            .map(|kept| kept.iter().copied().collect())
            .unwrap_or_default();
        tail(&String::from_utf8_lossy(&bytes))
    }
}

/// The trailing [`TAIL_CHARS`] of a stream, trimmed.
fn tail(text: &str) -> String {
    let text = text.trim();
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(TAIL_CHARS))
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn automation(
        action: crate::config::automations::Action,
    ) -> crate::config::automations::Automation {
        crate::config::automations::Automation {
            name: "nightly".into(),
            enabled: true,
            trigger: "cron:0 3 * * *".into(),
            timezone: None,
            prompt: "it's time; run `make`".into(),
            action,
            timeout_secs: None,
            created_at: 0,
            last_run_at: None,
            next_run_at: None,
            runs: Vec::new(),
        }
    }

    /// A send and a spawn reach Fleet through its link: the spawn's Task on
    /// stdin, then the prompt, as one word, to the session it made.
    #[test]
    fn a_fire_reaches_fleet_through_its_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "here=$(dirname \"$0\")\n\
             case \"$1\" in\n\
             apply) cat > \"$here/applied.json\" ;;\n\
             send) printf '%s\\n' \"$2=$3\" >> \"$here/sent\" ;;\n\
             esac",
        );
        let repo = tempfile::tempdir().expect("repo");
        let (send, spawn) = crate::paths::with_path(remote.path(), || {
            let send = fire(
                &remote.link,
                &automation(crate::config::automations::Action::Send {
                    session: "review".into(),
                }),
            );
            let spawn = fire(
                &remote.link,
                &automation(crate::config::automations::Action::Spawn {
                    repo: repo.path().to_path_buf(),
                    agent: None,
                }),
            );
            (send, spawn)
        });
        assert_eq!(
            send.status,
            crate::config::automations::RunStatus::Success,
            "{}",
            send.detail
        );
        assert_eq!(
            spawn.status,
            crate::config::automations::RunStatus::Success,
            "{}",
            spawn.detail
        );
        let task = spawn.session.expect("the session it made");
        let applied: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(remote.dir().join("remote/applied.json")).expect("applied"),
        )
        .expect("a Task on stdin");
        assert_eq!(applied["metadata"]["name"], task.as_str());
        assert_eq!(
            std::fs::read_to_string(remote.dir().join("remote/sent")).expect("sent"),
            format!("review=it's time; run `make`\n{task}=it's time; run `make`\n")
        );
        assert!(
            remote.called("apply --create-only -f -"),
            "{:?}",
            remote.calls()
        );
    }
}
