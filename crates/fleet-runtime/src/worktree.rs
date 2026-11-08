//! The git worktree workspace adapter: Fleet's own materialization of a
//! `{repositories}` workspace, as bounded `git` commands on the host.
//!
//! A workspace `key` lives in `<root>/<key>`, one worktree per repository at
//! `<root>/<key>/<name>`, each on the branch `<branchPrefix>/<key>` of its
//! source checkout. The session runs in the primary repository's worktree,
//! and its trust root is the primary's source checkout. The marker file
//! `<root>/<key>/.fleet-workspace.json` is written last: a directory without
//! it is a partial workspace, which `find` reports as none and the next
//! `materialize` rolls back.
//!
//! This adapter serves the local host. Each step is a `git` command bounded
//! through `fleet-exec`, so a later transport can run the same commands on a
//! remote host.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_api::Observation;
use fleet_api::plan::EffectOutcome;
use fleet_api::task::{LOCAL_HOST, RepositoriesWorkspace};
use fleet_api::workspace::{WorkspaceRef, Workspaces};
use fleet_exec::{Outcome, Spec, run};
use serde::{Deserialize, Serialize};

use crate::thurbox::Repositories;

/// The marker the adapter writes last into a whole workspace.
pub const MARKER: &str = ".fleet-workspace.json";

/// The most repositories one workspace may name here, so one call has a
/// bound the manager's lease can outlast.
pub const MAX_REPOSITORIES: usize = 4;

/// The most `git` commands one call runs: two per repository to roll back
/// a partial workspace, two to make it and two to roll that back.
const MAX_COMMANDS: u32 = 6 * MAX_REPOSITORIES as u32;

#[derive(Debug, Clone)]
pub struct WorktreeConfig {
    /// The `git` executable, a path or a name on PATH.
    pub git: OsString,
    /// The absolute directory that holds every workspace on the host.
    pub root: PathBuf,
    /// Each repository URL a Task may name, and its source checkout.
    pub repositories: Repositories,
    /// The first part of every workspace branch.
    pub branch_prefix: String,
    /// The bound on every `git` command.
    pub call_timeout: Duration,
}

impl WorktreeConfig {
    pub fn new(root: impl Into<PathBuf>, repositories: Repositories) -> WorktreeConfig {
        WorktreeConfig {
            git: "git".into(),
            root: root.into(),
            repositories,
            branch_prefix: "fleet".into(),
            call_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeWorkspaces {
    cfg: WorktreeConfig,
}

/// What the marker holds: the workspace, and each worktree's source, so
/// `clean` can unregister the worktrees.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Marker {
    workspace: WorkspaceRef,
    worktrees: Vec<Worktree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Worktree {
    path: String,
    source: String,
}

/// One `git` command's answer.
enum Git {
    Ok,
    /// git answered no; the first line it said.
    No(String),
    /// No answer within the bound.
    Unknown(String),
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.trim().chars().take(200).collect()
}

fn failed(why: impl Into<String>) -> Observation<EffectOutcome> {
    Observation::Known(EffectOutcome::Failed { why: why.into() })
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl WorktreeWorkspaces {
    pub fn new(cfg: WorktreeConfig) -> WorktreeWorkspaces {
        WorktreeWorkspaces { cfg }
    }

    fn git(&self, source: &str, args: &[&str]) -> Git {
        let spec = Spec::new(&self.cfg.git, self.cfg.call_timeout)
            .arg("-C")
            .arg(source)
            .args(args);
        match run(&spec) {
            Outcome::Exited { code: Some(0), .. } => Git::Ok,
            Outcome::Exited {
                code: Some(_),
                stderr,
                ..
            } => Git::No(first_line(&stderr)),
            other => Git::Unknown(format!("git {}: {}", args.join(" "), other.failure())),
        }
    }

    /// The workspace's own directory. A key that could name another
    /// directory is refused.
    fn dir_of(&self, key: &str) -> Result<PathBuf, String> {
        if key.is_empty() || key.starts_with('.') || key.contains(['/', '\\']) {
            return Err(format!("workspace key {key:?} is not a plain name"));
        }
        if !self.cfg.root.is_absolute() {
            return Err("workspaces.root must be an absolute path".into());
        }
        Ok(self.cfg.root.join(key))
    }

    fn read_marker(&self, dir: &Path) -> io::Result<Option<Marker>> {
        match fs::read(dir.join(MARKER)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Unregister each worktree and remove the directory: what a partial
    /// materialize and a clean both end with. Git's own answers are not
    /// needed: `prune` forgets a worktree whose directory is gone.
    fn remove(&self, dir: &Path, worktrees: &[Worktree]) -> io::Result<()> {
        for w in worktrees {
            let _ = self.git(&w.source, &["worktree", "remove", "--force", &w.path]);
        }
        match fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        for w in worktrees {
            let _ = self.git(&w.source, &["worktree", "prune"]);
        }
        Ok(())
    }

    /// Each repository's worktree and source checkout, or the reason there
    /// is none, before any command runs.
    fn plan(&self, dir: &Path, w: &RepositoriesWorkspace) -> Result<Vec<Worktree>, EffectOutcome> {
        if w.repositories.is_empty() || w.repositories.len() > MAX_REPOSITORIES {
            return Err(EffectOutcome::Failed {
                why: format!(
                    "the worktree adapter makes workspaces of 1 to {MAX_REPOSITORIES} repositories"
                ),
            });
        }
        w.repositories
            .iter()
            .map(|r| {
                let source = self
                    .cfg
                    .repositories
                    .source(&r.url)
                    .ok_or_else(|| EffectOutcome::Unmapped { url: r.url.clone() })?;
                if !Path::new(&source).is_absolute() {
                    return Err(EffectOutcome::Failed {
                        why: format!(
                            "runtime.repositories: the source {source:?} of {} must be an absolute path",
                            r.url
                        ),
                    });
                }
                Ok(Worktree {
                    path: text(&dir.join(&r.name)),
                    source,
                })
            })
            .collect()
    }

    fn make(
        &self,
        key: &str,
        dir: &Path,
        w: &RepositoriesWorkspace,
        trees: &[Worktree],
    ) -> Result<WorkspaceRef, Git> {
        let branch = format!("{}/{key}", self.cfg.branch_prefix);
        fs::create_dir_all(dir).map_err(|e| Git::No(format!("cannot create {dir:?}: {e}")))?;
        for (r, t) in w.repositories.iter().zip(trees) {
            let exists = format!("refs/heads/{branch}");
            // An earlier, rolled-back attempt may have left the branch: the
            // worktree goes back onto it, so no commit is lost.
            let answer = match self.git(&t.source, &["rev-parse", "--verify", "--quiet", &exists]) {
                Git::Ok => self.git(&t.source, &["worktree", "add", &t.path, &branch]),
                Git::No(_) => {
                    let mut args = vec!["worktree", "add", "-b", &branch, &t.path];
                    // Validation keeps a ref from starting with '-'.
                    if let Some(base) = &r.reference {
                        args.push(base.as_str());
                    }
                    self.git(&t.source, &args)
                }
                unknown => unknown,
            };
            match answer {
                Git::Ok => {}
                Git::No(why) => return Err(Git::No(format!("repository {}: {why}", r.name))),
                unknown => return Err(unknown),
            }
        }
        let workspace = WorkspaceRef {
            key: key.to_string(),
            host: LOCAL_HOST.to_string(),
            dir: trees[0].path.clone(),
            extra_dirs: trees[1..].iter().map(|t| t.path.clone()).collect(),
            trust_root: Some(trees[0].source.clone()),
        };
        let marker = Marker {
            workspace: workspace.clone(),
            worktrees: trees.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|e| Git::No(format!("marker encoding: {e}")))?;
        let tmp = dir.join(format!("{MARKER}.tmp"));
        fs::write(&tmp, bytes)
            .and_then(|()| fs::rename(&tmp, dir.join(MARKER)))
            .map_err(|e| Git::No(format!("cannot write the marker: {e}")))?;
        Ok(workspace)
    }
}

impl Workspaces for WorktreeWorkspaces {
    fn operation_bound(&self) -> fleet_api::Millis {
        let all = fleet_exec::worst_case(self.cfg.call_timeout) * MAX_COMMANDS;
        u64::try_from(all.as_millis()).unwrap_or(u64::MAX)
    }

    fn materialize(
        &self,
        host: &str,
        key: &str,
        w: &RepositoriesWorkspace,
    ) -> Observation<EffectOutcome> {
        if host != LOCAL_HOST {
            return failed(format!(
                "the worktree adapter makes workspaces on the local host only, not on {host}"
            ));
        }
        let dir = match self.dir_of(key) {
            Ok(d) => d,
            Err(why) => return failed(why),
        };
        let trees = match self.plan(&dir, w) {
            Ok(t) => t,
            Err(outcome) => return Observation::Known(outcome),
        };
        match Workspaces::find(self, host, key) {
            Observation::Known(Some(workspace)) => {
                return Observation::Known(EffectOutcome::Materialized {
                    workspace: Box::new(workspace),
                    adopted: true,
                });
            }
            Observation::Known(None) => {}
            Observation::Unknown { why } => return Observation::Unknown { why },
        }
        // Whole or nothing: a partial attempt goes before a new one starts.
        if dir.exists()
            && let Err(e) = self.remove(&dir, &trees)
        {
            return Observation::unknown(format!("cannot roll back {dir:?}: {e}"));
        }
        match self.make(key, &dir, w, &trees) {
            Ok(workspace) => Observation::Known(EffectOutcome::Materialized {
                workspace: Box::new(workspace),
                adopted: false,
            }),
            Err(answer) => {
                let why = match answer {
                    Git::No(why) | Git::Unknown(why) => why,
                    Git::Ok => String::new(),
                };
                match self.remove(&dir, &trees) {
                    Ok(()) => failed(why),
                    Err(e) => Observation::unknown(format!("{why}; cannot roll back: {e}")),
                }
            }
        }
    }

    fn find(&self, host: &str, key: &str) -> Observation<Option<WorkspaceRef>> {
        if host != LOCAL_HOST {
            return Observation::unknown(format!(
                "the worktree adapter cannot read workspaces on {host}"
            ));
        }
        let dir = match self.dir_of(key) {
            Ok(d) => d,
            Err(why) => return Observation::unknown(why),
        };
        match self.read_marker(&dir) {
            Ok(m) => Observation::Known(
                m.map(|m| m.workspace)
                    .filter(|w| w.key == key && w.host == host),
            ),
            Err(e) => Observation::unknown(format!("cannot read {dir:?}: {e}")),
        }
    }

    fn clean(&self, workspace: &WorkspaceRef) -> Observation<EffectOutcome> {
        if workspace.host != LOCAL_HOST {
            return failed(format!(
                "the worktree adapter cleans workspaces on the local host only, not on {}",
                workspace.host
            ));
        }
        let dir = match self.dir_of(&workspace.key) {
            Ok(d) => d,
            Err(why) => return failed(why),
        };
        let trees = match self.read_marker(&dir) {
            Ok(m) => m.map(|m| m.worktrees).unwrap_or_default(),
            Err(e) => return Observation::unknown(format!("cannot read {dir:?}: {e}")),
        };
        match self.remove(&dir, &trees) {
            Ok(()) => Observation::Known(EffectOutcome::Deleted),
            Err(e) => failed(format!("cannot remove {dir:?}: {e}")),
        }
    }
}
