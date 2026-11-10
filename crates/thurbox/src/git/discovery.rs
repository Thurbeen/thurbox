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

/// A repository's local branches and the one a new worktree starts from by
/// default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branches {
    /// The default base: the branch `origin/HEAD` names when the repository
    /// has that branch locally, else the branch checked out, else the first.
    /// `None` for a repository with no branch yet.
    pub default: Option<String>,
    /// Every local branch, sorted by name.
    pub names: Vec<String>,
}

/// The local branches of the repository at `repo` on this machine, or `None`
/// when `repo` is no git repository.
///
/// Local branches only: a `git+file` workspace is made from a clone of this
/// checkout, and that clone holds exactly these branches.
pub fn branches(repo: &str) -> Option<Branches> {
    let repo = paths::expand_tilde(repo);
    super::run_git_capture(&["rev-parse", "--git-dir"], &repo)?;
    let mut names: Vec<String> = super::run_git_capture(
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        &repo,
    )
    .unwrap_or_default()
    .lines()
    .map(str::trim)
    .filter(|name| !name.is_empty())
    .map(str::to_string)
    .collect();
    names.sort();
    let local = |name: &str| names.iter().any(|known| known == name);
    let advertised = super::run_git_capture(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        &repo,
    )
    .and_then(|out| out.trim().strip_prefix("origin/").map(str::to_string))
    .filter(|name| local(name));
    let checked_out = super::run_git_capture(&["symbolic-ref", "--short", "HEAD"], &repo)
        .map(|out| out.trim().to_string())
        .filter(|name| local(name));
    let default = advertised
        .or(checked_out)
        .or_else(|| names.first().cloned());
    Some(Branches { default, names })
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
