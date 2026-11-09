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
            // A confirmation is checked against the Task as it is now, the
            // way thurbox's engine checks it against the row.
            Command::Guarded {
                inner,
                session,
                backend_id,
                cwd,
                member_dirs,
            } => {
                let row = self
                    .get(session)?
                    .map(|task| crate::kernel::snapshot::fleet::row_of(&task));
                if row.is_none_or(|row| {
                    row.backend_id != *backend_id
                        || row.cwd != *cwd
                        || row.member_dirs != *member_dirs
                }) {
                    return Err("confirmation target changed".into());
                }
                return self.execute(inner, _id, _progress);
            }
            // The multiplexer the create flow settles on is the thurbox
            // engine's: Fleet's runtime keeps the terminal, so it is dropped.
            Command::Create {
                name,
                repo,
                branch,
                base,
                worktree_path,
                agent,
                host,
                multiplexer: _,
                extras,
            } => {
                let asked = [
                    (branch.is_some(), "a branch"),
                    (base.is_some(), "a base branch"),
                    (worktree_path.is_some(), "an existing worktree"),
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
                    "metadata": {"name": task_name(name)},
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

/// A session's name as a Task's: lowercase letters, digits, `-` and `.`,
/// starting and ending with a letter or digit, which is what Fleet takes.
/// Anything else becomes a `-`, and runs of them one.
fn task_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
}

impl Fleet {
    /// `fleet get task <name>`: the Task, or `None` when there is none.
    fn get(&self, name: &str) -> Result<Option<serde_json::Value>, String> {
        let out = Process::new(&self.program)
            .args(["get", "task", name])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not start fleet get: {e}"))?;
        match out.status.code() {
            Some(0) => serde_json::from_slice(&out.stdout)
                .map(Some)
                .map_err(|e| format!("fleet get answered no JSON: {e}")),
            // Not found: the Task is gone.
            Some(3) => Ok(None),
            _ => Err(format!(
                "fleet get: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

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
