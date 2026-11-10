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
            Command::Create {
                name,
                repo,
                agent,
                host,
            } => {
                let host = self.host(host.as_deref())?;
                if let Some(agent) = agent {
                    self.harness(agent)?;
                }
                // Fleet makes worktrees on its own machine only (its worktree
                // adapter refuses any other host), so a session elsewhere
                // runs in the repository directory there, as it is.
                let workspace = if host == "local" {
                    self.worktree_of(&crate::paths::expand_tilde(repo))?
                } else {
                    json!({"dir": repo})
                };
                let mut spec = json!({
                    "host": host,
                    "workspace": workspace,
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
            // Fleet has no undo window: a delete is for good.
            Command::Delete { session, .. } => self.run(&["delete", "task", session], None)?,
            // Everything else is a file of thurbox's, or a command no engine
            // runs: refused by name, never dropped.
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
pub(crate) fn task_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if c == '.' {
            // Fleet's names forbid `..`, so a run of dots is one.
            if !out.ends_with('.') {
                out.push(c);
            }
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
}

impl Fleet {
    /// Give the session `name`: the Task's [`NAME_LABEL`], set with `fleet
    /// label`, which changes that label and nothing else of the Task, so a
    /// Task a Conversation made keeps the labels only Fleet writes. The name
    /// is checked by the rules a created session's name is.
    ///
    /// [`NAME_LABEL`]: crate::kernel::snapshot::fleet::NAME_LABEL
    fn rename(&self, session: &str, name: &str) -> Result<(), String> {
        crate::paths::validate_safe_name(name)?;
        let label = format!("{}={name}", crate::kernel::snapshot::fleet::NAME_LABEL);
        self.run(&["label", "task", session, &label], None)
    }

    /// The Fleet host a session on `route` runs on: the picker's route
    /// (`ssh:devbox`) names the machine Fleet calls `devbox`, and no route
    /// is `local`. A host Fleet's config does not define is refused.
    fn host(&self, route: Option<&str>) -> Result<String, String> {
        let host = match route.filter(|route| !route.is_empty()) {
            None => "local".to_string(),
            Some(route) if crate::session::Route::parse(route).is_ok_and(|r| r.is_local()) => {
                "local".to_string()
            }
            // The flow's host rows carry Fleet's own host names.
            Some(route) if !route.contains(':') => route.to_string(),
            Some(route) => crate::session::host_of_key(route)
                .ok_or_else(|| format!("not a host: {route}"))?
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

    /// The `repositories` workspace for the checkout at `dir`, so Fleet
    /// makes the session a git worktree of its own. Fleet makes worktrees of
    /// mapped repositories only (`[workspaces.repositories]`, a URL to its
    /// checkout here), so an unmapped one is refused with the line to add,
    /// never run in the checkout instead.
    fn worktree_of(&self, dir: &std::path::Path) -> Result<serde_json::Value, String> {
        // A directory that is no git repository (the installed interface the
        // picker offers to edit) has no worktree to make: it runs as it is.
        if crate::git::repository_identity_on(None, dir).is_none() {
            return Ok(json!({"dir": dir}));
        }
        let answer = self.json(&["repositories", "--json"])?;
        let same = |source: &str| {
            let canonical = |path: &std::path::Path| {
                std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
            };
            canonical(std::path::Path::new(source)) == canonical(dir)
        };
        let url = answer["repositories"].as_object().and_then(|map| {
            map.iter()
                .find(|(_, source)| source.as_str().is_some_and(same))
                .map(|(url, _)| url.clone())
        });
        let Some(url) = url else {
            return Err(format!(
                "Fleet maps no repository at {dir}, so it cannot give the session a worktree: \
                 add \"<its URL>\" = \"{dir}\" under [workspaces.repositories] in Fleet's config",
                dir = dir.display()
            ));
        };
        // Fleet names a workspace's repository as it names a Task, so a
        // checkout called `My_Repo` is `my-repo`.
        let name = dir
            .file_name()
            .map(|n| task_name(&n.to_string_lossy()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "repo".to_string());
        Ok(json!({"repositories": [{"name": name, "url": url}]}))
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

#[cfg(test)]
mod tests {
    use super::task_name;

    /// Fleet's names forbid `..`, so a folder such as `app..backup` must not
    /// make one.
    #[test]
    fn a_name_never_holds_two_dots_in_a_row() {
        assert_eq!(task_name("app..backup"), "app.backup");
        assert_eq!(task_name("My_Repo"), "my-repo");
    }
}
