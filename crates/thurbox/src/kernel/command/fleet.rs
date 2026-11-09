//! Fleet as the write side (ADR-41): each command that changes a session is
//! one call of Fleet's command line, on the command worker that runs it.
//!
//! A session is a Task named as the session is. Creating one applies an
//! interactive Task (dispatched, no brief); sending queues a message in its
//! inbox; deleting marks it, and its controller releases the session. Fleet
//! decides everything after that, so thurbox only reports what the command
//! line answered. A session command Fleet has no verb for is refused by
//! name, never dropped.
//!
//! Only sessions are Fleet's. The interface's settings and its repository
//! memory are files of thurbox's ([`super::execute::local`]).

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
            // Fleet's runtime keeps the terminal, so the multiplexer the
            // create flow settles on is dropped.
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
                let host = self.host(host.as_deref())?;
                if let Some(agent) = agent {
                    self.harness(agent)?;
                }
                let mut spec = json!({
                    "host": host,
                    "workspace": {"dir": crate::paths::expand_tilde(repo)},
                    "dispatch": true,
                });
                if let Some(agent) = agent {
                    spec["harness"] = json!(agent);
                }
                let task_name = task_name(name);
                let mut task = json!({
                    "apiVersion": "fleet/v1alpha1",
                    "kind": "Task",
                    "metadata": {"name": task_name},
                    "spec": spec,
                });
                // The name as given, which the row is published under and a
                // finished creation finds it by.
                task["metadata"]["labels"][crate::kernel::snapshot::fleet::NAME_LABEL] =
                    json!(name);
                // Create-only: a plain apply would update a Task already
                // there, so two names that make one Task name (`Review`,
                // `review`) are refused rather than merged, by Fleet's
                // exclusive create even when two interfaces race.
                self.run(
                    &["apply", "--create-only", "-f", "-"],
                    Some(&task.to_string()),
                )?;
            }
            Command::Send { session, text } => self.run(&["send", session, text], None)?,
            Command::Rename { session, name } => self.rename(session, name)?,
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
            // Everything else is a file of thurbox's, or something no
            // engine runs any more: refused by name, never dropped.
            _ => {
                return super::execute::local(command).unwrap_or_else(|| {
                    Err(format!(
                        "{} is not available on the Fleet engine",
                        command.kind()
                    ))
                })
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
    /// Give the session `name`: the Task's [`NAME_LABEL`], applied back with
    /// its spec as it is. Fleet's apply replaces a Task's labels and keeps its
    /// amendments, so nothing else of the Task changes. An apply of the Task
    /// by someone else between this read and this apply is undone, which a
    /// rename from the keyboard accepts. The name is checked by the rules a
    /// created session's name is.
    ///
    /// [`NAME_LABEL`]: crate::kernel::snapshot::fleet::NAME_LABEL
    fn rename(&self, session: &str, name: &str) -> Result<(), String> {
        crate::paths::validate_safe_name(name)?;
        let mut task = self
            .get(session)?
            .ok_or_else(|| format!("no session {session}"))?;
        let label = &mut task["metadata"]["labels"][crate::kernel::snapshot::fleet::NAME_LABEL];
        if label.as_str() == Some(name) {
            return Ok(());
        }
        *label = json!(name);
        // Status is the controllers': an applied object must not carry it.
        if let Some(fields) = task.as_object_mut() {
            fields.remove("status");
        }
        self.run(&["apply", "-f", "-"], Some(&task.to_string()))
    }

    /// The Fleet host a session on `route` runs on: the picker's route
    /// (`ssh:devbox`) names the machine Fleet calls `devbox`, and no route
    /// is `local`. A host Fleet's config does not define is refused.
    fn host(&self, route: Option<&str>) -> Result<String, String> {
        let host = match route.filter(|route| !route.is_empty()) {
            None => "local".to_string(),
            Some(route) => crate::session::Route::parse(route)
                .map_err(|_| format!("not a host: {route}"))?
                .host()
                .unwrap_or("local")
                .to_string(),
        };
        let answer = self.json(&["runtime", "hosts", "--json"])?;
        // `null`: one runtime serves any host.
        if let Some(hosts) = answer["hosts"].as_array() {
            if !hosts.iter().any(|known| known == host.as_str()) {
                let known: Vec<&str> = hosts.iter().filter_map(|h| h.as_str()).collect();
                return Err(format!(
                    "Fleet defines no host {host} (its hosts: {}); add [hosts.{host}] to Fleet's config",
                    known.join(", ")
                ));
            }
        }
        Ok(host)
    }

    /// Refuse an agent Fleet has no harness profile for: Fleet would run a
    /// program named as the agent, which is not the command thurbox's
    /// registry runs for it (`shell` is `bash -i`).
    fn harness(&self, agent: &str) -> Result<(), String> {
        let answer = self.json(&["version", "--json"])?;
        let profiled: Vec<&str> = answer["harnesses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|h| h.as_str())
            .collect();
        if profiled.contains(&agent) {
            return Ok(());
        }
        Err(format!(
            "the agent {agent} has no Fleet harness profile; Fleet runs {}",
            profiled.join(", ")
        ))
    }

    /// `fleet <args>`'s JSON answer.
    fn json(&self, args: &[&str]) -> Result<serde_json::Value, String> {
        let out = Process::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not start fleet {}: {e}", args[0]))?;
        if !out.status.success() {
            return Err(format!(
                "fleet {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("fleet {} answered no JSON: {e}", args.join(" ")))
    }

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
