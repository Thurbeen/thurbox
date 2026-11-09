//! Fleet as the write side (ADR-41): each command that changes a session is
//! one call of Fleet's command line, on the command worker that runs it.
//!
//! A session is a Task named as the session is. Creating one applies an
//! interactive Task (dispatched, no brief); sending queues a message in its
//! inbox; deleting marks it, and its controller releases the session. Fleet
//! decides everything after that, so thurbox only reports what the command
//! line answered. What Fleet has no verb for is refused by name, never
//! dropped.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::mpsc::Sender;

use serde_json::json;

use super::bus::Progress;
use super::execute::Outcome;
use super::Command;

pub(super) struct Fleet {
    /// The `fleet` binary: a name on `PATH` or a path.
    pub(super) program: PathBuf,
}

impl super::Executor for Fleet {
    fn execute(
        &self,
        command: &Command,
        _id: u64,
        _progress: &Sender<Progress>,
    ) -> Result<Outcome, String> {
        match command {
            Command::Create {
                name,
                repo,
                branch,
                base,
                worktree_path,
                agent,
                host,
                multiplexer,
                extras,
            } => {
                let asked = [
                    (branch.is_some(), "a branch"),
                    (base.is_some(), "a base branch"),
                    (worktree_path.is_some(), "an existing worktree"),
                    (multiplexer.is_some(), "a multiplexer"),
                    (!extras.is_empty(), "further repositories"),
                ];
                if let Some((_, what)) = asked.iter().find(|(asked, _)| *asked) {
                    return Err(format!(
                        "the Fleet engine takes no {what}: Fleet makes a Task's workspace, and \
                         its runtime runs the terminal"
                    ));
                }
                let mut spec = json!({
                    "host": host.as_deref().unwrap_or("local"),
                    "workspace": {"dir": crate::paths::expand_tilde(repo)},
                    "dispatch": true,
                });
                if let Some(agent) = agent {
                    spec["harness"] = json!(agent);
                }
                let task = json!({
                    "apiVersion": "fleet/v1alpha1",
                    "kind": "Task",
                    "metadata": {"name": name},
                    "spec": spec,
                });
                self.run(&["apply", "-f", "-"], Some(&task.to_string()))?;
            }
            Command::Send { session, text } => self.run(&["send", session, text], None)?,
            // Fleet has no undo window: a soft delete is a delete.
            Command::Delete { session, .. } => self.run(&["delete", "task", session], None)?,
            // Fleet revives a lost session itself; the interface's relaunch of
            // a pane-less row has nothing to do.
            Command::Restart {
                fresh: false,
                if_missing: true,
                ..
            } => return Ok(Outcome::Nothing),
            // No undo window to sweep.
            Command::Reap => return Ok(Outcome::Nothing),
            // The interface's own files, whatever the engine.
            Command::Configure { settings } => {
                crate::config::settings::save_settings(settings)
                    .map_err(|e| format!("write settings.toml: {e}"))?;
            }
            other => {
                return Err(format!(
                    "{} is not available on the Fleet engine",
                    other.kind()
                ))
            }
        }
        Ok(Outcome::Done)
    }
}

impl Fleet {
    /// Run `fleet <args>`, with `input` on its stdin, and answer its error
    /// when it exits non-zero.
    fn run(&self, args: &[&str], input: Option<&str>) -> Result<(), String> {
        let mut child = Process::new(&self.program)
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
}
