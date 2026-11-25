use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use super::git_program;
use crate::paths;

pub(super) static REPO_NAME_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, String>>> =
    std::sync::OnceLock::new();

pub(super) fn repo_name_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    REPO_NAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

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

pub fn is_git_repo(path: &Path) -> bool {
    let git = path.join(".git");
    git.is_dir() || git.is_file()
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirListing {
    Missing,
    Entries(Vec<(String, bool)>),
}

pub fn list_dir_entries(dir: &str) -> DirListing {
    let dir = &paths::expand_tilde(dir);
    if !dir.is_dir() {
        return DirListing::Missing;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branches {
    pub default: Option<String>,
    pub names: Vec<String>,
    pub worktrees: Vec<Worktree>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
}

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

pub fn fetch_origin(repo: &str, deadline: std::time::Duration) -> Result<(), String> {
    let repo = paths::expand_tilde(repo);
    if super::run_git_capture(&["remote", "get-url", "origin"], &repo).is_none() {
        return Ok(());
    }
    // `followRemoteHEAD=always` moves `origin/HEAD` to the upstream default (git 2.48+; older git ignores it).
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

pub fn branches(repo: &str) -> Result<Branches, BranchesUnread> {
    let repo = paths::expand_tilde(repo);
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchesUnread {
    NotGit,
    Failed(String),
}

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

pub(super) fn parse_repo_name_from_url(url: &str) -> Option<String> {
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
