//! What one fire of an automation does, and the record it leaves.
//!
//! `send` types into the target's window; `spawn` creates a session headlessly
//! (the TUI adopts it on next startup) and delivers the prompt once the agent
//! boots; `exec` runs a command through [`super::super::bounded`], the runner
//! lifecycle hooks use, with a deadline, a directory and an environment of its
//! own (issue #1507).

use std::path::PathBuf;
use std::time::Duration;

use crate::session::{Automation, AutomationAction, AutomationRunStatus, SessionId};
use crate::storage::automations::RunRecord;
use crate::storage::Database;
use crate::sync::current_time_millis;

use super::super::deliver::{spawn_and_deliver, SpawnDeliverError};
use super::super::SpawnRequest;

/// Characters of each exec stream a run keeps.
const TAIL_CHARS: usize = 4096;

/// The session identity a pane gives its processes, which an exec never
/// inherits from whoever fired it.
const IDENTITY_VARS: [&str; 4] = [
    "THURBOX_SESSION",
    "THURBOX_SESSION_ID",
    "THURBOX_SESSION_KEY",
    "THURBOX_TASK",
];

/// An exec fire, detached from the database so it can run on a worker thread
/// while the tick goes on with everything else.
#[derive(Debug, Clone)]
pub struct ExecJob {
    pub automation_id: i64,
    pub name: String,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

impl ExecJob {
    /// The exec `auto` runs, or `None` for a send or spawn.
    pub fn of(auto: &Automation) -> Option<Self> {
        let AutomationAction::Exec { command } = &auto.action else {
            return None;
        };
        Some(Self {
            automation_id: auto.id,
            name: auto.name.clone(),
            command: command.clone(),
            cwd: auto.run.cwd.clone(),
            env: auto.run.env.clone(),
            timeout: auto.run.timeout(),
        })
    }

    /// Run it to its exit or its deadline.
    ///
    /// What it gets is the same whoever fires it — the heartbeat, an OS timer,
    /// a hand-run tick, `run --wait`: the firer's environment, then the
    /// automation's own variables, then the `THURBOX_*` location of this
    /// instance and the automation's identity, which win so a `thurbox-cli`
    /// inside reaches the instance that fired it. It runs in its `cwd`, or in
    /// the home directory, never in whatever directory the firer happened to
    /// be started in.
    pub fn run(self) -> RunRecord {
        use super::super::bounded::Exit;

        let mut cmd = super::super::platform_shell(&self.command);
        // A firer inside a pane carries that session's identity; the heartbeat
        // carries none. Dropping it keeps the two runs alike, and keeps a
        // `message send` in the job from being signed as the pane's agent.
        for var in IDENTITY_VARS {
            cmd.env_remove(var);
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)))
            .envs(super::super::thurbox_env_overrides())
            .env("THURBOX_AUTOMATION_ID", self.automation_id.to_string())
            .env("THURBOX_AUTOMATION_NAME", &self.name);
        if let Some(dir) = self.cwd.clone().or_else(crate::paths::home_dir) {
            cmd.current_dir(dir);
        }
        let done = super::super::bounded::run(cmd, None, self.timeout);
        let finished_at = current_time_millis();
        let stdout = tail(&done.stdout);
        let stderr = tail(&done.stderr);
        let output = summary(&stdout, &stderr);
        let (status, exit_code, detail) = match done.exit {
            Exit::Code(0) => (
                AutomationRunStatus::Success,
                Some(0),
                if output.is_empty() {
                    "ok".into()
                } else {
                    output
                },
            ),
            Exit::Code(code) => (
                AutomationRunStatus::Error,
                Some(code),
                with_output(format!("exit {code}"), &output),
            ),
            Exit::Signal => (
                AutomationRunStatus::Error,
                None,
                with_output("exit signal".into(), &output),
            ),
            Exit::TimedOut(timeout) => (
                AutomationRunStatus::TimedOut,
                None,
                with_output(format!("timed out after {}s", timeout.as_secs()), &output),
            ),
            Exit::SpawnFailed(e) => (
                AutomationRunStatus::Error,
                None,
                format!("spawn failed: {e}"),
            ),
            Exit::WaitFailed(e) => (
                AutomationRunStatus::Error,
                None,
                format!("could not be waited on: {e}"),
            ),
        };
        RunRecord {
            status,
            detail,
            related_session: None,
            finished_at,
            exit_code,
            stdout_tail: Some(stdout),
            stderr_tail: Some(stderr),
        }
    }
}

/// The trailing [`TAIL_CHARS`] of a stream, trimmed.
fn tail(stream: &[u8]) -> String {
    let text = String::from_utf8_lossy(stream);
    let text = text.trim();
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(TAIL_CHARS))
        .collect()
}

/// The two tails as the one-line `detail` has always shown them: stdout, then
/// stderr.
fn summary(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (_, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

fn with_output(head: String, output: &str) -> String {
    match output.is_empty() {
        true => head,
        false => format!("{head}: {output}"),
    }
}

/// Fire a send or spawn automation and say how it went. An exec goes through
/// [`ExecJob`] instead, which needs no database; handed one here, it runs on
/// this thread.
pub fn fire(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    auto: &Automation,
) -> RunRecord {
    let plain = |status, detail: String, related| {
        RunRecord::plain(status, detail, related, current_time_millis())
    };
    match &auto.action {
        AutomationAction::Send { session_id } => {
            let (status, detail, related) = fire_send(db, backends, auto, *session_id);
            plain(status, detail, related)
        }
        AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            base_branch,
            agent,
            extra_repos,
        } => {
            let req = SpawnRequest {
                repo_path: repo_path.clone(),
                worktree_branch: worktree_branch.clone(),
                base_branch: base_branch.clone(),
                agent: agent.clone(),
                extra_repos: extra_repos.clone(),
                ..Default::default()
            };
            let (status, detail, related) = fire_spawn(db, backends, auto, req);
            plain(status, detail, related)
        }
        AutomationAction::Exec { .. } => match ExecJob::of(auto) {
            Some(job) => job.run(),
            None => unreachable!("an exec automation always yields its job"),
        },
    }
}

/// Type the prompt into the target session's window, on the backend its route
/// names.
fn fire_send(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    auto: &Automation,
    session_id: SessionId,
) -> (AutomationRunStatus, String, Option<SessionId>) {
    // The full row, not just the name: the persisted pane id is what
    // disambiguates when another session shares the name.
    let target = match db.get_session_by_id(session_id) {
        Ok(Some(session)) => session,
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
                format!("get_session_by_id: {e}"),
                None,
            )
        }
    };
    match super::super::windows::agent_pane(backends, &target) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                AutomationRunStatus::Skipped,
                "target session not running".into(),
                None,
            )
        }
        Err(e) => return (AutomationRunStatus::Error, e, None),
    }
    match super::super::send_text_with_status(db, backends, &target, &auto.prompt, true) {
        Ok(()) => (
            AutomationRunStatus::Success,
            format!("sent to {session_id}"),
            Some(session_id),
        ),
        Err(e) => (AutomationRunStatus::Error, format!("{e:#}"), None),
    }
}

/// Reuse the session an earlier fire made, or spawn a new headless one, then
/// deliver the prompt once the agent boots.
fn fire_spawn(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    auto: &Automation,
    req: SpawnRequest,
) -> (AutomationRunStatus, String, Option<SessionId>) {
    let name = format!("auto-{}", auto.id);
    // Reuse the session an earlier fire made, found by its row and on the
    // backend its route names — never a window by its name alone: one no row
    // owns, or several that answer to it, is somebody else's to type into.
    let sessions = match db.find_sessions_by_name(&name) {
        Ok(sessions) => sessions,
        Err(e) => {
            return (
                AutomationRunStatus::Error,
                format!("find_sessions_by_name: {e}"),
                None,
            )
        }
    };
    let mut running = Vec::new();
    for session in sessions {
        // An automation spawns on this machine, so only a session here can be
        // one it made: a same-named row on a host is another machine's (a
        // mirrored peer's automation shares the name). And a row on a route
        // nothing here serves is not a session this process could be reusing.
        if crate::session::Route::is_remote_key(&session.backend_type)
            || super::super::windows::backend_for(backends, &session.backend_type).is_err()
        {
            continue;
        }
        match super::super::windows::agent_pane(backends, &session) {
            Ok(Some(_)) => running.push(session),
            Ok(None) => {}
            // Not knowing whether it runs must not launch a second one.
            Err(e) => return (AutomationRunStatus::Error, e, None),
        }
    }
    match running.as_slice() {
        [] => {}
        [session] => {
            return match super::super::send_text_with_status(
                db,
                backends,
                session,
                &auto.prompt,
                true,
            ) {
                Ok(()) => (
                    AutomationRunStatus::Success,
                    format!("reused {name}"),
                    Some(session.id),
                ),
                Err(e) => (AutomationRunStatus::Error, format!("{e:#}"), None),
            }
        }
        _ => {
            return (
                AutomationRunStatus::Error,
                format!(
                    "{} running sessions are named {name}, so there is no telling which one \
                     this automation made",
                    running.len()
                ),
                None,
            )
        }
    }
    let req = SpawnRequest {
        name: name.clone(),
        ..req
    };
    match spawn_and_deliver(db, backends, &name, req, &auto.prompt) {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn job(command: &str) -> ExecJob {
        ExecJob {
            automation_id: 9,
            name: "probe".into(),
            command: command.into(),
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn a_quiet_success_reads_ok_and_a_failure_keeps_its_code() {
        let ok = job("true").run();
        assert_eq!(ok.status, AutomationRunStatus::Success);
        assert_eq!(ok.detail, "ok");
        assert_eq!(ok.exit_code, Some(0));

        let failed = job("printf hello; exit 3").run();
        assert_eq!(failed.status, AutomationRunStatus::Error);
        assert_eq!(failed.exit_code, Some(3));
        assert_eq!(failed.detail, "exit 3: hello");
        assert_eq!(failed.stdout_tail.as_deref(), Some("hello"));
    }

    #[test]
    fn a_tail_keeps_the_last_characters_of_a_long_stream() {
        let run = job("yes x | head -c 20000; echo; echo END").run();
        let stdout = run.stdout_tail.expect("a tail");
        assert!(stdout.chars().count() <= TAIL_CHARS, "{}", stdout.len());
        assert!(stdout.ends_with("END"), "{stdout:?}");
    }

    #[test]
    fn the_automation_s_variables_never_shadow_the_instance() {
        let mut job = job("printf '%s|%s' \"$MINE\" \"$THURBOX_AUTOMATION_ID\"");
        job.env = vec![
            ("MINE".into(), "yes".into()),
            ("THURBOX_AUTOMATION_ID".into(), "spoofed".into()),
        ];
        assert_eq!(job.run().stdout_tail.as_deref(), Some("yes|9"));
    }
}
