//! The git worktree workspace adapter: Fleet's own materialization of a
//! `{repositories}` workspace, as bounded `git` commands on the host. It is
//! the default adapter: Fleet makes every repositories workspace.
//!
//! A workspace `key` lives in `<root>/<key>`, one worktree per repository at
//! `<root>/<key>/<name>`, each on the branch `<branchPrefix>/<key>` of its
//! source checkout. A `file://` repository is a directory used as it is: no
//! worktree, and never removed. The session runs in the primary repository's
//! directory, and the trust root of a worktree is its source checkout. The
//! marker file `<root>/<key>/.fleet-workspace.json` is written last: a
//! directory without it is a partial workspace, which `find` reports as none
//! and the next `materialize` rolls back.
//!
//! A clean never loses work: a worktree with changes its branch does not
//! hold, untracked files included, is committed
//! onto `refs/fleet/preserved/<key>/<name>` in its source before the
//! worktree goes. Branches are never deleted.
//!
//! This adapter serves the local host. Each step is a `git` command bounded
//! through `fleet-exec`, so a later transport can run the same commands on a
//! remote host.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use fleet_api::Observation;
use fleet_api::plan::EffectOutcome;
use fleet_api::task::{LOCAL_HOST, RepositoriesWorkspace};
use fleet_api::workspace::{WorkspaceRef, Workspaces};
use fleet_exec::{Outcome, Spec, run};
use serde::{Deserialize, Serialize};

/// The repository map, shared: whoever holds a clone may replace it while
/// the adapter runs, and the next materialize reads the new map. `fleet up`
/// does so when its config file changes.
#[derive(Debug, Clone, Default)]
pub struct Repositories(Arc<RwLock<std::collections::BTreeMap<String, String>>>);

impl Repositories {
    pub fn new(map: std::collections::BTreeMap<String, String>) -> Repositories {
        Repositories(Arc::new(RwLock::new(map)))
    }

    pub(crate) fn source(&self, url: &str) -> Option<String> {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(url)
            .cloned()
    }

    /// Replace the whole map; true when it changed.
    pub fn replace(&self, map: std::collections::BTreeMap<String, String>) -> bool {
        let mut current = self.0.write().unwrap_or_else(|e| e.into_inner());
        let changed = *current != map;
        *current = map;
        changed
    }
}

/// The variables that point git at another repository than `-C` names; a
/// parent process (a git hook, a test) may have set them.
pub const GIT_LOCATION: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

/// The ref namespace preserved work goes to, per workspace key and name.
pub const PRESERVED: &str = "refs/fleet/preserved";

/// The marker the adapter writes last into a whole workspace.
pub const MARKER: &str = ".fleet-workspace.json";

/// The most repositories one workspace may name here, so one call has a
/// bound the manager's lease can outlast.
pub const MAX_REPOSITORIES: usize = 4;

/// The most `git` commands one call runs, per repository: a materialize
/// runs two to roll back a partial workspace, two to make it and two to roll
/// that back; a clean three to preserve its work and two to remove it.
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
    /// The source checkout; empty for a directory used as it is.
    source: String,
    /// The repository's name in the Task: the preserved ref's last part.
    #[serde(default)]
    name: String,
}

impl Worktree {
    fn as_is(&self) -> bool {
        self.source.is_empty()
    }
}

/// The directory a `file://` URL names, when it is an absolute path.
fn file_url(url: &str) -> Option<Result<String, String>> {
    let rest = url.strip_prefix("file://")?;
    // `file:///C:/x` names `C:/x` on Windows.
    let b = rest.as_bytes();
    let path = if b.len() > 3 && b[0] == b'/' && b[2] == b':' {
        &rest[1..]
    } else {
        rest
    };
    Some(if Path::new(path).is_absolute() {
        Ok(path.to_string())
    } else {
        Err(format!("{url} does not name an absolute path"))
    })
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
        match self.git_out(source, args, &[]) {
            Ok(_) => Git::Ok,
            Err(answer) => answer,
        }
    }

    /// A `git` command's first line of output, or its answer when it failed.
    fn git_out(&self, dir: &str, args: &[&str], env: &[(&str, &str)]) -> Result<String, Git> {
        let mut spec = Spec::new(&self.cfg.git, self.cfg.call_timeout)
            .arg("-C")
            .arg(dir)
            .args(args);
        spec.env_remove = GIT_LOCATION.iter().map(|v| v.to_string()).collect();
        spec.env = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Err(match run(&spec) {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => return Ok(first_line(&stdout)),
            Outcome::Exited {
                code: Some(_),
                stderr,
                ..
            } => Git::No(first_line(&stderr)),
            other => Git::Unknown(format!("git {}: {}", args.join(" "), other.failure())),
        })
    }

    /// Commit what `w` holds that its branch does not, untracked files
    /// included, onto its preserved ref in the source: the worktree's index
    /// takes everything, and `stash create` commits it without touching a
    /// ref or the files. The worktree goes next, so its index is no loss.
    /// Nothing to keep is `Ok`.
    fn preserve(&self, key: &str, w: &Worktree) -> Result<(), Git> {
        if w.as_is() || !Path::new(&w.path).exists() {
            return Ok(());
        }
        let who = [
            ("GIT_AUTHOR_NAME", "Fleet"),
            ("GIT_AUTHOR_EMAIL", "fleet@localhost"),
            ("GIT_COMMITTER_NAME", "Fleet"),
            ("GIT_COMMITTER_EMAIL", "fleet@localhost"),
        ];
        self.git_out(&w.path, &["add", "-A"], &[])?;
        let message = format!("fleet: work preserved from workspace {key}");
        let commit = self.git_out(&w.path, &["stash", "create", &message], &who)?;
        if commit.is_empty() {
            return Ok(());
        }
        let name = format!("{PRESERVED}/{key}/{}", w.name);
        self.git_out(&w.path, &["update-ref", &name, &commit], &[])
            .map(|_| ())
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
        let worktrees: Vec<&Worktree> = worktrees.iter().filter(|w| !w.as_is()).collect();
        for w in &worktrees {
            let _ = self.git(&w.source, &["worktree", "remove", "--force", &w.path]);
        }
        match fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        for w in &worktrees {
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
                if let Some(path) = file_url(&r.url) {
                    let path = path.map_err(|why| EffectOutcome::Failed { why })?;
                    if !Path::new(&path).is_dir() {
                        return Err(EffectOutcome::Failed {
                            why: format!("{} is not a directory", r.url),
                        });
                    }
                    return Ok(Worktree {
                        path,
                        source: String::new(),
                        name: r.name.clone(),
                    });
                }
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
                    name: r.name.clone(),
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
            if t.as_is() {
                continue;
            }
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
            trust_root: (!trees[0].as_is()).then(|| trees[0].source.clone()),
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
        for t in &trees {
            match self.preserve(&workspace.key, t) {
                Ok(()) => {}
                Err(Git::No(why)) => {
                    return failed(format!("cannot preserve the work in {}: {why}", t.path));
                }
                Err(Git::Unknown(why)) => {
                    return Observation::unknown(format!(
                        "preserving the work in {}: {why}",
                        t.path
                    ));
                }
                Err(Git::Ok) => {}
            }
        }
        match self.remove(&dir, &trees) {
            Ok(()) => Observation::Known(EffectOutcome::Deleted),
            Err(e) => failed(format!("cannot remove {dir:?}: {e}")),
        }
    }
}
