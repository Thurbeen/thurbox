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

use std::process::Stdio;
use std::sync::mpsc::Sender;

use serde_json::json;

use super::bus::Progress;
use super::execute::Outcome;
use super::Command;

pub(super) struct Fleet {
    /// The Fleet's command line.
    pub(super) link: crate::fleet_link::FleetLink,
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
                    .get(self.link.task(session)?)?
                    .map(|task| crate::kernel::snapshot::fleet::row_of(&self.link, &task));
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
                worktree,
                branch,
                base,
                extras,
            } => {
                let host = self.host(host.as_deref())?;
                if let Some(agent) = agent {
                    self.harness(agent)?;
                }
                let session = task_name(name);
                let mut urls = vec![workspace_url(
                    &host,
                    repo,
                    Choice {
                        worktree: *worktree,
                        branch: branch.as_deref(),
                        base: base.as_deref(),
                    },
                    &session,
                )?];
                // Every other repository is on the same branch, from its own
                // default: the base asked for is the first repository's.
                for extra in extras {
                    urls.push(workspace_url(
                        &host,
                        &extra.path,
                        Choice {
                            worktree: Some(extra.worktree),
                            branch: branch.as_deref(),
                            base: None,
                        },
                        &session,
                    )?);
                }
                let workspace = json!(urls);
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
                self.link.run(
                    &["apply", "--create-only", "-f", "-"],
                    Some(&task.to_string()),
                )?;
            }
            Command::Send { session, text } => self
                .link
                .run(&["send", self.link.task(session)?, text], None)?,
            Command::Rename { session, name } => self.rename(self.link.task(session)?, name)?,
            // Fleet has no undo window: a delete is for good.
            Command::Delete { session, .. } => self
                .link
                .run(&["delete", "task", self.link.task(session)?], None)?,
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
        self.link.run(&["label", "task", session, &label], None)
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
                    "Fleet has no place {host} (its places: {}); a place is `<runtime>/<place>`: add [runtimes.<name>.places.<place>] to Fleet's config",
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
        let out = self
            .link
            .command(args)
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
        let out = self
            .link
            .command(&["get", "task", name])
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
}

/// What the creation flow chose for a session's workspace.
#[derive(Debug, Clone, Copy, Default)]
struct Choice<'a> {
    /// A worktree (`git+file`) or the directory as it is (`file`). `None`:
    /// a worktree for a git repository on this machine, the directory
    /// otherwise.
    worktree: Option<bool>,
    /// The worktree's branch. `None`: a new branch named after the session.
    branch: Option<&'a str>,
    /// What a new branch starts from. `None`: the repository's default.
    base: Option<&'a str>,
}

/// The workspace URL of a session over `repo` on the Fleet host `host`.
///
/// A worktree is `git+file://<repo>?branch=<branch>#<base>`: the runtime
/// makes the worktree on the branch, from the base when the branch is new.
/// The directory as it is is `file://<repo>`. A path on another host must be
/// absolute, because only that host could expand a `~`, and Fleet takes no
/// relative URL. Whether a host's runtime makes worktrees at all is its own:
/// Fleet refuses a scheme the runtime does not list.
fn workspace_url(
    host: &str,
    repo: &str,
    choice: Choice<'_>,
    session: &str,
) -> Result<String, String> {
    let local = host == "local";
    let dir = if local {
        crate::paths::expand_tilde(repo)
    } else {
        std::path::PathBuf::from(repo)
    };
    // Another host's path is read as that host reads it, which need not be
    // this machine's way: `/srv/app` is absolute on a Linux host even when
    // thurbox runs on Windows.
    let absolute = if local {
        dir.is_absolute()
    } else {
        repo.starts_with('/') || has_drive(repo)
    };
    if !absolute {
        return Err(if local {
            format!("not an absolute path: {repo}")
        } else {
            format!("a path on {host} must be absolute, not {repo}")
        });
    }
    let worktree = match choice.worktree {
        Some(true) if local && !is_git(&dir) => {
            return Err(format!(
                "{} is no git repository, so it has no worktree",
                dir.display()
            ))
        }
        Some(chosen) => chosen,
        None => local && is_git(&dir),
    };
    if !worktree {
        return Ok(dir_url("file", &dir));
    }
    let mut url = dir_url("git+file", &dir);
    url.push_str("?branch=");
    url.push_str(&encode(choice.branch.unwrap_or(session)));
    if let Some(base) = choice.base {
        url.push('#');
        url.push_str(&encode(base));
    }
    Ok(url)
}

/// Whether `dir` on this machine is a git repository or inside one. Decided
/// from the filesystem, not from asking `git`, so a `git` that cannot run
/// never passes for a directory with no repository. A bare repository keeps
/// its files where a checkout keeps `.git`.
fn is_git(dir: &std::path::Path) -> bool {
    let bare =
        dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir();
    bare || dir.ancestors().any(|at| at.join(".git").exists())
}

/// Whether `path` starts with a Windows drive, `C:\\` or `C:/`.
fn has_drive(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\')
}

/// A path as a URL spells it: `/` separators, and a Windows drive after a
/// leading `/` (`C:\\x` is `/C:/x`); Windows' `\\?\` prefix dropped.
fn url_path(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    if has_drive(path) {
        format!("/{}", path.replace('\\', "/"))
    } else {
        path.to_string()
    }
}

/// `<scheme>://<dir>`, with the bytes a URL reads apart (`%`, `?`, `#` and
/// whitespace) percent-encoded, as the runtime decodes them.
pub(crate) fn dir_url(scheme: &str, dir: &std::path::Path) -> String {
    let mut url = format!("{scheme}://");
    for c in url_path(&dir.to_string_lossy()).chars() {
        push_char(&mut url, c, |c| {
            c == '%' || c == '?' || c == '#' || c.is_whitespace()
        });
    }
    url
}

/// A branch or base as a query value or fragment: also `&` and `=`, which
/// would end or split the value, are percent-encoded.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        push_char(&mut out, c, |c| {
            matches!(c, '%' | '?' | '#' | '&' | '=') || c.is_whitespace()
        });
    }
    out
}

fn push_char(out: &mut String, c: char, reserved: impl Fn(char) -> bool) {
    if reserved(c) {
        let mut buf = [0; 4];
        for b in c.encode_utf8(&mut buf).bytes() {
            out.push_str(&format!("%{b:02X}"));
        }
    } else {
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::{task_name, workspace_url, Choice};

    /// Every command the executor runs reaches Fleet through its link: the
    /// reads a creation checks against (`runtime hosts`, `version`), the
    /// Task on stdin, free text as one word, a rename, a delete, and a
    /// confirmation's `get`, whose exit 3 still reads as a Task gone.
    #[cfg(unix)]
    #[test]
    fn every_command_reaches_fleet_through_its_link() {
        use super::super::{Command, Executor};
        let remote = crate::fleet_link::testing::Remote::new(
            "here=$(dirname \"$0\")\n\
             case \"$1\" in\n\
             runtime) echo '{\"hosts\":[\"local\"]}' ;;\n\
             version) echo '{\"harnesses\":[\"claude\"]}' ;;\n\
             apply) cat > \"$here/applied.json\" ;;\n\
             send) printf '%s' \"$3\" > \"$here/sent\" ;;\n\
             get) exit 3 ;;\n\
             esac",
        );
        let repo = tempfile::tempdir().expect("repo");
        let fleet = super::Fleet {
            link: remote.link.clone(),
        };
        let (progress, _) = std::sync::mpsc::channel();
        let run = |command: Command| fleet.execute(&command, 1, &progress).map(|_| ());
        crate::paths::with_path(remote.path(), || {
            run(Command::Create {
                name: "review".into(),
                repo: repo.path().display().to_string(),
                agent: Some("claude".into()),
                host: None,
                worktree: Some(false),
                branch: None,
                base: None,
                extras: Vec::new(),
            })
            .expect("create");
            let text = "it's \"done\"; $HOME `x`";
            run(Command::Send {
                session: "review".into(),
                text: text.into(),
            })
            .expect("send");
            run(Command::Rename {
                session: "devbox:review".into(),
                name: "audit".into(),
            })
            .expect("a row's id names the Task in its Fleet");
            let elsewhere = run(Command::Delete {
                session: "laptop:review".into(),
                force: false,
            })
            .expect_err("a session of another Fleet");
            assert!(elsewhere.contains("Fleet laptop"), "{elsewhere}");
            run(Command::Delete {
                session: "review".into(),
                force: false,
            })
            .expect("delete");
            let guarded = run(Command::Guarded {
                inner: Box::new(Command::Delete {
                    session: "review".into(),
                    force: false,
                }),
                session: "review".into(),
                backend_id: None,
                cwd: None,
                member_dirs: Vec::new(),
            });
            assert_eq!(guarded, Err("confirmation target changed".to_string()));
            assert_eq!(
                std::fs::read_to_string(remote.dir().join("remote/sent")).expect("sent"),
                text
            );
        });
        let applied: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(remote.dir().join("remote/applied.json")).expect("applied"),
        )
        .expect("a Task on stdin");
        assert_eq!(applied["metadata"]["name"], "review");
        for call in [
            "runtime hosts --json",
            "version --json",
            "apply --create-only -f -",
            "send review",
            "label task review thurbox.dev/name=audit",
            "delete task review",
            "get task review",
        ] {
            assert!(remote.called(call), "{call}: {:?}", remote.calls());
        }
    }

    /// A worktree names its branch, the session's by default, and its base
    /// when one was chosen; the directory as it is takes neither.
    #[test]
    fn a_workspace_url_carries_the_branch_and_base_chosen() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(repo.path().join(".git")).expect(".git");
        let dir = repo.path().display().to_string();
        let new = |branch, base| Choice {
            worktree: Some(true),
            branch,
            base,
        };
        assert_eq!(
            workspace_url("local", &dir, new(None, Some("main")), "wt").unwrap(),
            format!("git+file://{}?branch=wt#main", super::url_path(&dir))
        );
        assert_eq!(
            workspace_url("local", &dir, new(Some("feat/a&b"), None), "wt").unwrap(),
            format!("git+file://{}?branch=feat/a%26b", super::url_path(&dir))
        );
        assert_eq!(
            workspace_url("local", &dir, Choice::default(), "wt").unwrap(),
            format!("git+file://{}?branch=wt", super::url_path(&dir))
        );
        let off = Choice {
            worktree: Some(false),
            ..Choice::default()
        };
        assert_eq!(
            workspace_url("local", &dir, off, "wt").unwrap(),
            format!("file://{}", super::url_path(&dir))
        );
    }

    /// A directory with no repository has no worktree to make, and a path on
    /// another host must be absolute: only that host knows its `~`.
    #[test]
    fn a_workspace_url_refuses_what_no_runtime_could_make() {
        let plain = tempfile::tempdir().expect("tempdir");
        let dir = plain.path().display().to_string();
        let on = Choice {
            worktree: Some(true),
            ..Choice::default()
        };
        assert!(workspace_url("local", &dir, on, "wt")
            .unwrap_err()
            .contains("no git repository"));
        assert_eq!(
            workspace_url("local", &dir, Choice::default(), "wt").unwrap(),
            format!("file://{}", super::url_path(&dir))
        );
        assert!(workspace_url("devbox", "~/app", Choice::default(), "wt")
            .unwrap_err()
            .contains("must be absolute"));
        assert_eq!(
            workspace_url("devbox", "/srv/app", Choice::default(), "wt").unwrap(),
            "file:///srv/app"
        );
    }

    /// A Windows path is spelled as a URL spells one, so a runtime reads it
    /// back to the same directory.
    #[test]
    fn a_windows_path_is_spelled_with_its_drive_after_a_slash() {
        use super::{has_drive, url_path};
        assert_eq!(url_path(r"C:\Users\me\app"), "/C:/Users/me/app");
        assert_eq!(url_path(r"\\?\C:\x"), "/C:/x");
        assert_eq!(url_path("/srv/app"), "/srv/app");
        assert!(has_drive("D:/w") && !has_drive("/srv"));
    }

    /// Fleet's names forbid `..`, so a folder such as `app..backup` must not
    /// make one.
    #[test]
    fn a_name_never_holds_two_dots_in_a_row() {
        assert_eq!(task_name("app..backup"), "app.backup");
        assert_eq!(task_name("My_Repo"), "my-repo");
    }
}
