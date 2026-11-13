//! Answering "what is here" on this machine: directory listings and
//! child-repo scans for the creation flow, and repository display names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use super::git_program;
use crate::paths;

/// Global cache for repo display names (path → name).
pub(super) static REPO_NAME_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, String>>> =
    std::sync::OnceLock::new();

pub(super) fn repo_name_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    REPO_NAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get a short display name for a repo directory.
///
/// Tries to extract the repo name from `git remote get-url origin`
/// (e.g. `github.com/user/thurbox.git` → `"thurbox"`).
/// Falls back to the directory's file name if no remote is found.
/// Results are cached globally.
pub fn repo_display_name(path: &Path) -> Option<String> {
    let cache = repo_name_cache();
    if let Ok(guard) = cache.lock() {
        if let Some(name) = guard.get(path) {
            return Some(name.clone());
        }
    }
    let name = repo_name_from_remote(path).or_else(|| {
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_string)
    })?;
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), name.clone());
    }
    Some(name)
}

/// True if `path` is itself a git repo: it contains a `.git` directory OR a
/// `.git` file (worktree checkouts and submodules use a `.git` file).
pub fn is_git_repo(path: &Path) -> bool {
    let git = path.join(".git");
    git.is_dir() || git.is_file()
}

/// Immediate child directories of `parent` that are git repos, sorted by file
/// name. Hidden (`.`-prefixed) entries are skipped. Returns an empty vec when
/// `parent` can't be read (missing, permission denied, …).
pub fn scan_child_repos(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut repos: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter(|e| {
            !e.file_name()
                .to_str()
                .map(|n| n.starts_with('.'))
                .unwrap_or(true)
        })
        .map(|e| e.path())
        .filter(|p| is_git_repo(p))
        .collect();
    repos.sort();
    repos
}

/// Immediate sub-directories of a path, each flagged as a git repo or not —
/// the repo picker's path-browser payload. `Missing` is "the dir isn't there"
/// (a user typo, reported inline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirListing {
    /// The requested directory does not exist on the target filesystem.
    Missing,
    /// `(name, is_git)` per immediate sub-directory, sorted by name. Hidden
    /// (`.`-prefixed) entries are included — the picker's filter decides
    /// their visibility (offered only to a `.`-prefix, like the local
    /// completer).
    Entries(Vec<(String, bool)>),
}

/// List the immediate sub-directories of `dir` on this machine, flagging each
/// as a git repo the way [`is_git_repo`] does. A leading `~` expands against
/// this machine's home.
pub fn list_dir_entries(dir: &str) -> DirListing {
    let dir = &paths::expand_tilde(dir);
    if !dir.is_dir() {
        return DirListing::Missing;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Exists but unreadable (permissions): an empty listing keeps the
        // browser usable; the path itself may still be committed.
        return DirListing::Entries(Vec::new());
    };
    let mut names: Vec<(String, bool)> = entries
        .filter_map(Result::ok)
        // `path().is_dir()` follows symlinks to a directory.
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let is_git = is_git_repo(&e.path());
            Some((name, is_git))
        })
        .collect();
    names.sort();
    DirListing::Entries(names)
}

/// A repository's branches and the one a new worktree starts from by
/// default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branches {
    /// The default base: the upstream's default branch as `origin` has it
    /// (`origin/HEAD`, else `origin/main`, else `origin/master`), else the
    /// branch checked out, else the first. `None` for a repository with no
    /// branch yet.
    pub default: Option<String>,
    /// Every local branch, sorted by name, then every `origin/*` branch.
    pub names: Vec<String>,
    /// The worktrees the repository already has besides its own checkout,
    /// in git's order: a session can open one as it is.
    pub worktrees: Vec<Worktree>,
}

/// A worktree a repository already has: where it is checked out, and its
/// branch (`None` when its HEAD is detached).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
}

/// The worktrees in `git worktree list --porcelain` output, less the first
/// record: that one is the repository's own checkout (or, bare, no checkout
/// at all), which is the repository row itself.
pub fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    porcelain
        .split("\n\n")
        .skip(1)
        .filter_map(|record| {
            let mut path = None;
            let mut branch = None;
            for line in record.lines() {
                if let Some(at) = line.strip_prefix("worktree ") {
                    path = Some(at.to_string());
                } else if let Some(name) = line.strip_prefix("branch ") {
                    branch = Some(name.strip_prefix("refs/heads/").unwrap_or(name).to_string());
                } else if line == "bare" {
                    return None;
                }
            }
            Some(Worktree {
                path: path?,
                branch,
            })
        })
        .collect()
}

/// Fetch the `origin` of the repository at `repo` on this machine, so its
/// `refs/remotes/origin/*` and `origin/HEAD` are the upstream's as they are
/// now. A repository
/// with no `origin` has nothing to fetch.
///
/// Bounded by `deadline`: an ssh `origin` has a connect timeout and
/// keepalives ([`crate::shell::ssh_appended_opts`]) unless the user configured
/// their own ssh command, and an http one gives up
/// when it has moved no bytes for that long, so a host that accepts the
/// connection and never answers costs the picker `deadline`, not forever.
pub fn fetch_origin(repo: &str, deadline: std::time::Duration) -> Result<(), String> {
    let repo = paths::expand_tilde(repo);
    if super::run_git_capture(&["remote", "get-url", "origin"], &repo).is_none() {
        return Ok(());
    }
    // `followRemoteHEAD=always` moves `origin/HEAD` to what the upstream
    // advertises in the same fetch, so an upstream that changed its default
    // branch is followed (git 2.48+; an older git ignores it).
    let mut fetch = super::git_command(
        &repo,
        &[
            "-c",
            "remote.origin.followRemoteHEAD=always",
            "fetch",
            "-q",
            "origin",
        ],
    );
    // A repository's own ssh command (`core.sshCommand`, `GIT_SSH_COMMAND`,
    // `GIT_SSH`: a wrapper, a key, a proxy) is kept, so its fetch works as the
    // user's own does; only one nothing configures gets thurbox's options.
    let own_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some()
        || std::env::var_os("GIT_SSH").is_some()
        || super::run_git_capture(&["config", "--get", "core.sshCommand"], &repo)
            .is_some_and(|command| !command.trim().is_empty());
    if own_ssh {
        fetch.env("GIT_TERMINAL_PROMPT", "0");
    } else {
        super::non_interactive(&mut fetch);
    }
    fetch.env("GIT_HTTP_LOW_SPEED_LIMIT", "1").env(
        "GIT_HTTP_LOW_SPEED_TIME",
        deadline.as_secs().max(1).to_string(),
    );
    super::run_git(fetch, "git fetch")
        .map_err(|e| e.to_string().lines().next().unwrap_or_default().to_string())
}

/// The branches of the repository at `repo` on this machine, as its refs
/// hold them, or why there are none. [`fetch_origin`] first for the
/// upstream's current branches.
///
/// A `git+file` workspace is a worktree of this checkout, so every branch
/// listed here, remote-tracking ones included, is one it can start from.
pub fn branches(repo: &str) -> Result<Branches, BranchesUnread> {
    let repo = paths::expand_tilde(repo);
    // Whether there is a repository is read from the filesystem, so a `git`
    // that cannot run, or refuses the repository, never passes for none.
    let bare = repo.join("HEAD").is_file() && repo.join("objects").is_dir();
    if !bare && !repo.ancestors().any(|at| at.join(".git").exists()) {
        return Err(BranchesUnread::NotGit);
    }
    super::run_git_capture(&["rev-parse", "--git-dir"], &repo)
        .ok_or_else(|| BranchesUnread::Failed(format!("git could not read {}", repo.display())))?;
    let refs = super::run_git_capture(
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
        &repo,
    )
    .unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    let mut remote: Vec<String> = Vec::new();
    for line in refs.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix("refs/heads/") {
            names.push(name.to_string());
        } else if let Some(name) = line.strip_prefix("refs/remotes/origin/") {
            if name != "HEAD" {
                remote.push(format!("origin/{name}"));
            }
        }
    }
    names.sort();
    remote.sort();
    let local = |name: &str| names.iter().any(|known| known == name);
    let upstream = super::run_git_capture(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        &repo,
    )
    .map(|out| out.trim().to_string())
    .into_iter()
    .chain(["origin/main".to_string(), "origin/master".to_string()])
    .find(|name| remote.contains(name));
    let checked_out = super::run_git_capture(&["symbolic-ref", "--short", "HEAD"], &repo)
        .map(|out| out.trim().to_string())
        .filter(|name| local(name));
    let default = upstream.or(checked_out).or_else(|| names.first().cloned());
    names.extend(remote);
    let worktrees = super::run_git_capture(&["worktree", "list", "--porcelain"], &repo)
        .map(|out| parse_worktrees(&out))
        .unwrap_or_default();
    Ok(Branches {
        default,
        names,
        worktrees,
    })
}

/// Why [`branches`] has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchesUnread {
    /// The directory is no git repository.
    NotGit,
    /// It is one, and `git` could not read it: the message says why.
    Failed(String),
}

/// Parse repo name from the origin remote URL.
pub(super) fn repo_name_from_remote(path: &Path) -> Option<String> {
    let output = git_program()
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_repo_name_from_url(&url)
}

/// Extract repo name from a git remote URL.
///
/// Handles common formats:
/// - `git@github.com:user/repo.git` → `"repo"`
/// - `https://github.com/user/repo.git` → `"repo"`
/// - `https://github.com/user/repo` → `"repo"`
pub(super) fn parse_repo_name_from_url(url: &str) -> Option<String> {
    // Split on the last '/' (HTTPS/SSH with path) or ':' (SSH shorthand).
    let after_sep = if url.contains('/') {
        url.rsplit('/').next()?
    } else {
        url.rsplit(':').next()?
    };
    let name = after_sep.strip_suffix(".git").unwrap_or(after_sep);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
