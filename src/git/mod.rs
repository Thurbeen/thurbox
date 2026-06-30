use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

use crate::paths;
use crate::session::HostDef;
use crate::shell::{posix_quote, ssh_command, wsl_command};

/// Build the launcher [`Command`] for an off-local host: `ssh <opts> <dest>`
/// for an SSH host, or `wsl.exe -d <distro>` for a WSL distro. Both join and
/// shell-interpret the trailing tokens identically, so callers append the same
/// POSIX-quoted command afterward.
fn host_launcher(h: &HostDef) -> Command {
    if h.is_wsl() {
        wsl_command(&h.distro_name())
    } else {
        ssh_command(&h.destination, &h.ssh_opts)
    }
}

/// Build a `git` [`Command`] targeting `cwd`, run locally, over SSH, or inside
/// a WSL distro.
///
/// For an off-local host the command becomes
/// `<launcher> git -C <cwd> <args…>` (launcher = `ssh <opts> <dest>` or
/// `wsl.exe -d <distro>`), with each token shell-escaped so it survives the
/// host login shell's re-splitting.
fn git_command(host: Option<&HostDef>, cwd: &Path, args: &[&str]) -> Command {
    match host {
        None => {
            let mut cmd = Command::new("git");
            cmd.current_dir(cwd);
            cmd.args(args);
            cmd
        }
        Some(h) => {
            let mut cmd = host_launcher(h);
            cmd.arg(posix_quote("git"));
            cmd.arg(posix_quote("-C"));
            cmd.arg(posix_quote(&cwd.to_string_lossy()));
            for a in args {
                cmd.arg(posix_quote(a));
            }
            cmd
        }
    }
}

/// Cache of resolved host `$HOME` directories, keyed by the host's backend name
/// (`ssh:<name>` / `wsl:<name>`), so we only pay one round-trip per host. A WSL
/// host has no `destination`, so the backend name — unique per host — is the
/// stable key for both kinds. Entries live for the process lifetime:
/// `hosts.toml` is read once at startup, so repointing a host requires a
/// restart anyway — the cache can never be staler than the config it derives
/// from.
fn remote_home_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the `$HOME` directory on a host (over SSH or inside a WSL distro),
/// caching the result.
fn remote_home(host: &HostDef) -> Result<String> {
    let key = host.backend_name();
    if let Ok(guard) = remote_home_cache().lock() {
        if let Some(home) = guard.get(&key) {
            return Ok(home.clone());
        }
    }
    let mut cmd = host_launcher(host);
    // Pass `$HOME` literally; the host login shell expands it.
    cmd.arg("echo").arg("$HOME");
    let output = cmd
        .stderr(Stdio::piped())
        .output()
        .context("failed to resolve host $HOME")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`echo $HOME` on {key} failed: {}", stderr.trim());
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if home.is_empty() {
        anyhow::bail!("$HOME resolved empty for {key}");
    }
    if let Ok(mut guard) = remote_home_cache().lock() {
        guard.insert(key, home.clone());
    }
    Ok(home)
}

/// Deterministic worktree directory path for a repo + branch on the given host.
///
/// Local hosts use [`worktree_path`]. Remote hosts place worktrees under the
/// host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees` resolved
/// over ssh), preserving the same `<repo-hash>/<sanitized-branch>` layout.
fn worktree_path_for(host: Option<&HostDef>, repo_path: &Path, branch: &str) -> Result<PathBuf> {
    match host {
        None => worktree_path(repo_path, branch).context("failed to resolve worktrees directory"),
        Some(h) => {
            let base = match &h.worktrees_dir {
                Some(dir) => dir.clone(),
                None => format!("{}/.local/share/thurbox/worktrees", remote_home(h)?),
            };
            // The host is remote (always POSIX), so the path must be `/`-joined
            // even when thurbox itself runs on Windows — `PathBuf::join` would
            // otherwise insert `\` and produce a path the remote shell rejects.
            Ok(PathBuf::from(worktree_subpath_posix(
                &base, repo_path, branch,
            )))
        }
    }
}

/// Global cache for repo display names (path → name).
static REPO_NAME_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, String>>> =
    std::sync::OnceLock::new();

fn repo_name_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
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
            .and_then(|f| f.to_str())
            .map(|s| s.to_string())
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
        .filter_map(|e| e.ok())
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

/// Parse repo name from the origin remote URL.
fn repo_name_from_remote(path: &Path) -> Option<String> {
    let output = Command::new("git")
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
fn parse_repo_name_from_url(url: &str) -> Option<String> {
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

/// List local branch names for a repo.
pub fn list_branches(repo_path: &Path) -> Result<Vec<String>> {
    list_branches_on(None, repo_path)
}

/// [`list_branches`], optionally on a remote `host`.
pub fn list_branches_on(host: Option<&HostDef>, repo_path: &Path) -> Result<Vec<String>> {
    let output = git_command(host, repo_path, &["branch", "--format=%(refname:short)"])
        .output()
        .context("failed to run git branch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git branch failed: {stderr}");
    }

    let branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(branches)
}

/// Raw unified `git diff <base>..HEAD` output for a worktree, for the native
/// code-review view, optionally on a remote `host` (via `ssh <dest> git …`).
/// Returns `None` on failure (not a git dir, bad base, …); the caller falls
/// back to a narrower range or surfaces a status.
///
/// `--no-color` keeps the output parseable; the result is fed to
/// [`crate::session::parse_unified_diff`].
pub fn diff_against_on(host: Option<&HostDef>, worktree: &Path, base: &str) -> Option<String> {
    let range = format!("{base}..HEAD");
    run_diff(host, worktree, &["diff", "--no-color", &range])
}

/// Raw unified diff of the worktree's **uncommitted** changes vs `HEAD`
/// (staged + unstaged), for the review view's "working changes" target.
pub fn diff_working_on(host: Option<&HostDef>, worktree: &Path) -> Option<String> {
    run_diff(host, worktree, &["diff", "--no-color", "HEAD"])
}

/// Raw unified diff of a single commit (`git show`), for the review view's
/// per-commit target. `--format=` suppresses the log message, leaving the patch.
pub fn show_commit_on(host: Option<&HostDef>, worktree: &Path, sha: &str) -> Option<String> {
    run_diff(host, worktree, &["show", "--no-color", "--format=", sha])
}

/// List the commits in `<base>..HEAD` as `(short-sha, subject)`, newest first —
/// the choices for the review view's per-commit target picker.
pub fn list_commits_on(
    host: Option<&HostDef>,
    worktree: &Path,
    base: &str,
) -> Vec<(String, String)> {
    let range = format!("{base}..HEAD");
    let Some(out) = run_diff(
        host,
        worktree,
        &["log", "--no-color", "--format=%h%x09%s", &range],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, subj)| (sha.to_string(), subj.to_string()))
        .collect()
}

/// Run a `git` command and capture stdout, logging + returning `None` on
/// failure. Shared by the review-view diff/log helpers.
///
/// `core.quotepath=false` keeps non-ASCII paths verbatim (git otherwise
/// C-quotes them, e.g. `"caf\303\251.rs"`), so the parser keys comments/marks on
/// the real UTF-8 path.
fn run_diff(host: Option<&HostDef>, worktree: &Path, args: &[&str]) -> Option<String> {
    let mut full = vec!["-c", "core.quotepath=false"];
    full.extend_from_slice(args);
    let output = git_command(host, worktree, &full).output().ok()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("git {args:?} failed: {}", stderr.trim());
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Create a git worktree on a new branch and return the worktree directory path.
///
/// Creates `new_branch` starting from `base_branch`.
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
pub fn create_worktree(repo_path: &Path, new_branch: &str, base_branch: &str) -> Result<PathBuf> {
    create_worktree_on(None, repo_path, new_branch, base_branch)
}

/// [`create_worktree`], optionally on a remote `host` (via `ssh <dest> git …`).
/// On a remote host the worktree is created under the host's remote worktrees
/// directory and the returned path is a remote path.
pub fn create_worktree_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    new_branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let wt_path = worktree_path_for(host, repo_path, new_branch)?;

    let output = git_command(
        host,
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            new_branch,
            &wt_path.display().to_string(),
            base_branch,
        ],
    )
    .output()
    .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {stderr}");
    }

    Ok(wt_path)
}

/// Idempotently provision a worktree on `branch`, returning its directory.
///
/// Unlike [`create_worktree`] (which always passes `-b` and fails if the branch
/// already exists), this is safe to call repeatedly — the case that recurring
/// **spawn automations** hit on every fire:
///
/// - worktree directory already present → reuse it as-is;
/// - branch exists but has no worktree → attach a worktree to it
///   ([`add_existing_worktree`]);
/// - neither exists → create the branch + worktree off `base_branch`.
pub fn create_or_attach_worktree(
    repo_path: &Path,
    branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, branch).context("failed to resolve worktrees directory")?;
    if wt_path.exists() {
        return Ok(wt_path);
    }
    if branch_exists(repo_path, branch) {
        return add_existing_worktree(repo_path, branch);
    }
    create_worktree(repo_path, branch, base_branch)
}

/// Remove a git worktree (force removal).
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    remove_worktree_on(None, repo_path, worktree_path)
}

/// [`remove_worktree`], optionally on a remote `host`.
pub fn remove_worktree_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    worktree_path: &Path,
) -> Result<()> {
    let output = git_command(
        host,
        repo_path,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.display().to_string(),
        ],
    )
    .output()
    .context("failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree remove failed: {stderr}");
    }

    Ok(())
}

/// Detect the repository's default branch name.
///
/// Tries `git symbolic-ref refs/remotes/origin/HEAD` first (most reliable),
/// then falls back to checking for `main` or `master` among local branches.
pub fn default_branch(repo_path: &Path, local_branches: &[String]) -> Option<String> {
    default_branch_on(None, repo_path, local_branches)
}

/// [`default_branch`], optionally on a remote `host`.
pub fn default_branch_on(
    host: Option<&HostDef>,
    repo_path: &Path,
    local_branches: &[String],
) -> Option<String> {
    if let Some(name) = default_branch_from_remote_on(host, repo_path) {
        if local_branches.iter().any(|b| b == &name) {
            return Some(name);
        }
    }

    for candidate in ["main", "master"] {
        if local_branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

/// Query the remote's default branch via `git symbolic-ref`.
pub fn default_branch_from_remote(repo_path: &Path) -> Option<String> {
    default_branch_from_remote_on(None, repo_path)
}

/// [`default_branch_from_remote`], optionally on a remote `host`.
pub fn default_branch_from_remote_on(host: Option<&HostDef>, repo_path: &Path) -> Option<String> {
    let output = git_command(
        host,
        repo_path,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    )
    .stderr(Stdio::null())
    .output()
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let full_ref = String::from_utf8_lossy(&output.stdout).trim().to_string();
    full_ref.strip_prefix("origin/").map(|s| s.to_string())
}

/// Add an existing branch as a worktree (no `-b` flag — branch must already exist).
///
/// Returns the worktree directory path. If the worktree path already exists on
/// disk the function returns early with `Ok(path)`.
pub fn add_existing_worktree(repo_path: &Path, branch: &str) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, branch).context("failed to resolve worktrees directory")?;

    if wt_path.exists() {
        return Ok(wt_path);
    }

    let output = Command::new("git")
        .args(["worktree", "add", &wt_path.display().to_string(), branch])
        .current_dir(repo_path)
        .output()
        .context("failed to run git worktree add (existing branch)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add (existing) failed: {stderr}");
    }

    Ok(wt_path)
}

/// Check whether a local branch exists in the repository.
pub fn branch_exists(repo_path: &Path, branch: &str) -> bool {
    branch_exists_on(None, repo_path, branch)
}

/// [`branch_exists`], optionally on a remote `host`.
pub fn branch_exists_on(host: Option<&HostDef>, repo_path: &Path, branch: &str) -> bool {
    git_command(host, repo_path, &["rev-parse", "--verify", branch])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Deterministic worktree directory path for a repo + branch.
///
/// Worktrees are placed under the XDG data directory to avoid being inside
/// the source repo (which would cause Claude Code to discover duplicate
/// `.claude/commands/` skill files).
///
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
fn worktree_path(repo_path: &Path, branch: &str) -> Option<PathBuf> {
    Some(worktree_subpath(
        paths::worktrees_directory()?,
        repo_path,
        branch,
    ))
}

/// Stable 64-bit FNV-1a hash of `input`, rendered as 16 lowercase hex digits.
///
/// This is the `<repo-hash>` segment of a worktree path. It must be a **fixed**
/// algorithm: `std::collections::hash_map::DefaultHasher` (SipHash) is
/// explicitly documented as *not* guaranteed stable across Rust versions or
/// builds, so the same repo path could hash to different directories after a
/// toolchain bump — orphaning every persisted worktree. FNV-1a is a tiny,
/// dependency-free, fully specified algorithm, so a given path always maps to
/// the same directory.
fn stable_repo_hash(input: &str) -> String {
    // FNV-1a 64-bit constants (offset basis + prime).
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &byte in input.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// The two load-bearing path segments of the worktree layout: the stable
/// repo-path hash and the sanitized branch name. Both the local
/// ([`worktree_subpath`]) and remote ([`worktree_subpath_posix`]) variants
/// derive their `<repo-hash>/<sanitized-branch>` tail from this, so the
/// hash+sanitize logic lives in one place.
fn worktree_segments(repo_path: &Path, branch: &str) -> (String, String) {
    let repo_hash = stable_repo_hash(&repo_path.display().to_string());
    let sanitized = branch.replace('/', "-");
    (repo_hash, sanitized)
}

/// The deterministic `<base>/<repo-hash>/<sanitized-branch>` worktree layout,
/// shared by local ([`worktree_path`]) and remote ([`worktree_path_for`])
/// resolution so both produce identical sub-paths under their own base.
fn worktree_subpath(base: PathBuf, repo_path: &Path, branch: &str) -> PathBuf {
    let (repo_hash, sanitized) = worktree_segments(repo_path, branch);
    base.join(repo_hash).join(sanitized)
}

/// The same `<base>/<repo-hash>/<sanitized-branch>` layout as [`worktree_subpath`],
/// but rendered as a POSIX (`/`-joined) string for a **remote** host. This is
/// separate from the `PathBuf` form because on Windows `PathBuf::join` inserts
/// `\`, which the remote login shell would not accept.
fn worktree_subpath_posix(base: &str, repo_path: &Path, branch: &str) -> String {
    let (repo_hash, sanitized) = worktree_segments(repo_path, branch);
    format!("{}/{repo_hash}/{sanitized}", base.trim_end_matches('/'))
}

/// Result of attempting to sync a worktree with its base ref.
#[derive(Debug)]
pub enum SyncResult {
    /// Rebase succeeded (includes already-up-to-date).
    Synced,
    /// Rebase failed due to conflicts (aborted, stash restored).
    Conflict(String),
    /// Unexpected failure.
    Error(String),
}

/// Stash uncommitted changes. Returns `true` if anything was stashed.
fn git_stash(worktree_path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["stash"])
        .current_dir(worktree_path)
        .output()
        .context("failed to run git stash")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git stash failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(!stdout.contains("No local changes to save"))
}

/// Fetch from origin.
pub fn git_fetch(worktree_path: &Path) -> Result<()> {
    git_fetch_on(None, worktree_path)
}

/// [`git_fetch`], optionally on a remote `host`.
pub fn git_fetch_on(host: Option<&HostDef>, worktree_path: &Path) -> Result<()> {
    let output = git_command(host, worktree_path, &["fetch", "origin"])
        .output()
        .context("failed to run git fetch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git fetch failed: {stderr}");
    }

    Ok(())
}

/// Rebase the current branch onto `base_ref`. Returns `Ok(())` on success,
/// or an error if there are conflicts (rebase is aborted before returning).
fn git_rebase_onto(worktree_path: &Path, base_ref: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["rebase", base_ref])
        .current_dir(worktree_path)
        .output()
        .context("failed to run git rebase")?;

    if !output.status.success() {
        let _ = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(worktree_path)
            .output();

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rebase conflict: {stderr}");
    }

    Ok(())
}

/// Pop the most recent stash entry.
fn git_stash_pop(worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["stash", "pop"])
        .current_dir(worktree_path)
        .output()
        .context("failed to run git stash pop")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git stash pop failed: {stderr}");
    }

    Ok(())
}

/// Check whether a git error message indicates a transient index-lock failure.
fn is_transient_error(msg: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "could not write index",
        "Unable to write new index file",
        "index.lock': File exists",
        "Another git process seems to be running",
    ];
    PATTERNS.iter().any(|p| msg.contains(p))
}

/// Find the shared git directory for a worktree (handles linked worktrees).
fn git_common_dir(worktree_path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree_path)
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = Path::new(&dir);
    // git may return a relative path; resolve it against the worktree
    if path.is_absolute() {
        Some(PathBuf::from(dir))
    } else {
        Some(worktree_path.join(dir))
    }
}

/// Age threshold for mtime-based stale lock removal.
const STALE_LOCK_AGE: Duration = Duration::from_secs(60);

/// Remove a stale `index.lock` if we can confirm no live process holds it.
///
/// On Linux: reads the PID from the lock file content (if present) and checks `/proc/{pid}`.
/// Fallback (all platforms): removes if the lock file's mtime exceeds [`STALE_LOCK_AGE`].
fn cleanup_stale_index_lock(worktree_path: &Path) {
    let Some(git_dir) = git_common_dir(worktree_path) else {
        return;
    };
    let lock_path = git_dir.join("index.lock");
    if !lock_path.exists() {
        return;
    }

    #[cfg(target_os = "linux")]
    if try_remove_by_pid(&lock_path) {
        return;
    }

    try_remove_by_age(&lock_path);
}

/// Attempt to remove a lock file by checking if the owning PID is still alive.
///
/// Returns `true` if the PID was parseable (regardless of removal outcome),
/// meaning the caller should not fall through to the mtime-based check.
#[cfg(target_os = "linux")]
fn try_remove_by_pid(lock_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(lock_path) else {
        return false;
    };
    let Some(pid_str) = content.split_whitespace().next() else {
        return false;
    };
    let Ok(pid) = pid_str.parse::<u32>() else {
        return false;
    };

    if Path::new(&format!("/proc/{pid}")).exists() {
        return true;
    }

    if std::fs::remove_file(lock_path).is_ok() {
        warn!(
            "Removed stale index.lock (dead PID {pid}) at {}",
            lock_path.display()
        );
    }
    true
}

/// Remove a lock file if its mtime exceeds [`STALE_LOCK_AGE`].
fn try_remove_by_age(lock_path: &Path) {
    let age = std::fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());

    let Some(age) = age else { return };

    if age > STALE_LOCK_AGE && std::fs::remove_file(lock_path).is_ok() {
        warn!(
            "Removed stale index.lock (age {:?}) at {}",
            age,
            lock_path.display()
        );
    }
}

/// Per-attempt delays for `stash_with_retry`. The first entry (zero) is the
/// initial attempt; subsequent entries are the backoff delays before each retry.
const STASH_ATTEMPT_DELAYS: &[Duration] = &[
    Duration::ZERO,
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_secs(1),
];

/// Run `git stash` with retries on transient index-lock errors.
///
/// Returns `Ok(true)` if changes were stashed, `Ok(false)` if nothing to stash.
fn stash_with_retry(worktree_path: &Path) -> Result<bool> {
    let max_retries = STASH_ATTEMPT_DELAYS.len() - 1;
    let mut last_err = String::new();

    for (attempt, delay) in STASH_ATTEMPT_DELAYS.iter().enumerate() {
        if attempt > 0 {
            warn!(
                "Retrying git stash (retry {}/{}) in {}",
                attempt,
                max_retries,
                worktree_path.display()
            );
            std::thread::sleep(*delay);
            cleanup_stale_index_lock(worktree_path);
        }
        match git_stash(worktree_path) {
            Ok(stashed) => return Ok(stashed),
            Err(e) => {
                let msg = format!("{e:#}");
                if !is_transient_error(&msg) {
                    anyhow::bail!("{msg}");
                }
                last_err = msg;
            }
        }
    }

    anyhow::bail!("transient error persisted after retries: {last_err}")
}

/// Resolve the base ref a worktree should be compared/rebased against.
///
/// Prefers the branch's configured upstream (`@{upstream}`), then the remote's
/// advertised default branch (`origin/HEAD`, e.g. `origin/main` or
/// `origin/master` — whatever the repo actually uses), then the conventional
/// `origin/main` / `origin/master`. Returns `None` when none resolve, so the
/// caller can surface a clear error instead of blindly rebasing onto a
/// possibly-missing `origin/main`. Shared by [`sync_worktree`] (the rebase
/// target) and [`ahead_behind`] (the comparison base) so the "behind" count is
/// always measured against the ref sync would rebase onto.
fn resolve_base_ref(worktree_path: &Path) -> Option<String> {
    // A branch with an explicit upstream rebases onto exactly that.
    if run_git_capture(
        &["rev-parse", "--verify", "--quiet", "@{upstream}"],
        worktree_path,
    )
    .is_some()
    {
        return Some("@{upstream}".to_string());
    }

    // The remote's advertised HEAD (origin/HEAD → e.g. "origin/main").
    if let Some(out) = run_git_capture(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        worktree_path,
    ) {
        let advertised = out.trim();
        if !advertised.is_empty() {
            return Some(advertised.to_string());
        }
    }

    // Conventional fallbacks.
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|r| {
            run_git_capture(&["rev-parse", "--verify", "--quiet", r], worktree_path).is_some()
        })
        .map(str::to_string)
}

/// High-level sync: stash, fetch, rebase onto the base ref, pop stash.
///
/// `base_ref` pins the ref to rebase onto; when `None` it is derived from the
/// worktree via `resolve_base_ref` (upstream → `origin/HEAD` →
/// `origin/main` → `origin/master`) rather than hardcoding `origin/main`.
/// On conflict the rebase is aborted and any stash is restored.
/// Retries `git stash` on transient index-lock errors.
pub fn sync_worktree(worktree_path: &Path, base_ref: Option<&str>) -> SyncResult {
    cleanup_stale_index_lock(worktree_path);

    let stashed = match stash_with_retry(worktree_path) {
        Ok(s) => s,
        Err(e) => return SyncResult::Error(format!("stash: {e:#}")),
    };

    let restore_stash = || {
        if stashed {
            let _ = git_stash_pop(worktree_path);
        }
    };

    if let Err(e) = git_fetch(worktree_path) {
        restore_stash();
        return SyncResult::Error(format!("fetch: {e:#}"));
    }

    // Resolve the rebase target after the fetch so derived refs (origin/HEAD,
    // origin/main, …) reflect the just-fetched remote state.
    let base_ref = match base_ref
        .map(str::to_string)
        .or_else(|| resolve_base_ref(worktree_path))
    {
        Some(r) => r,
        None => {
            restore_stash();
            return SyncResult::Error(
                "could not resolve a base ref to sync onto (no upstream, \
                 origin/HEAD, origin/main, or origin/master)"
                    .to_string(),
            );
        }
    };

    if let Err(e) = git_rebase_onto(worktree_path, &base_ref) {
        restore_stash();
        return SyncResult::Conflict(format!("{e:#}"));
    }

    if stashed {
        if let Err(e) = git_stash_pop(worktree_path) {
            return SyncResult::Error(format!("stash pop: {e:#}"));
        }
    }

    SyncResult::Synced
}

/// Run `git <args>` in `cwd`, returning stdout on success (`None` otherwise).
fn run_git_capture(args: &[&str], cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Sum `git diff --numstat` output into `(files_changed, insertions, deletions)`.
/// Binary files (`-\t-\tpath`) count toward `files_changed` with zero lines.
fn parse_numstat(out: &str) -> (usize, usize, usize) {
    let (mut files, mut ins, mut dels) = (0usize, 0usize, 0usize);
    for line in out.lines() {
        let mut cols = line.split('\t');
        let added = cols.next();
        let deleted = cols.next();
        let path = cols.next();
        if path.is_none() {
            continue;
        }
        files += 1;
        if let Some(n) = added.and_then(|s| s.parse::<usize>().ok()) {
            ins += n;
        }
        if let Some(n) = deleted.and_then(|s| s.parse::<usize>().ok()) {
            dels += n;
        }
    }
    (files, ins, dels)
}

/// Commits the worktree's HEAD is `(ahead, behind)` relative to its base ref,
/// resolved by `resolve_base_ref` (upstream → `origin/HEAD` → `origin/main` →
/// `origin/master`) — the same chain [`sync_worktree`] rebases onto, so the
/// "behind" count is measured against the ref sync would use. Returns `(0, 0)`
/// when no base can be resolved.
pub fn ahead_behind(cwd: &Path) -> (usize, usize) {
    let Some(base) = resolve_base_ref(cwd) else {
        return (0, 0);
    };
    // `--left-right --count <base>...HEAD` → "<behind>\t<ahead>".
    let range = format!("{base}...HEAD");
    let Some(out) = run_git_capture(&["rev-list", "--left-right", "--count", &range], cwd) else {
        return (0, 0);
    };
    let mut parts = out.split_whitespace();
    let behind = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (ahead, behind)
}

/// Compute combined git stats (uncommitted diff + dirty + ahead/behind) for a
/// worktree. Returns `None` when the path is not a usable git worktree.
pub fn worktree_stats(cwd: &Path) -> Option<crate::session::GitStats> {
    // Bail early if this path isn't inside a git work tree.
    run_git_capture(&["rev-parse", "--is-inside-work-tree"], cwd)?;
    let numstat = run_git_capture(&["diff", "--numstat", "HEAD"], cwd).unwrap_or_default();
    let (files_changed, insertions, deletions) = parse_numstat(&numstat);
    // One `status --porcelain` drives both dirty (any output) and the untracked
    // count (`??` entries) — files a worktree removal would lose but that `diff
    // HEAD` never reports.
    let status = run_git_capture(&["status", "--porcelain"], cwd).unwrap_or_default();
    let dirty = !status.trim().is_empty();
    let untracked = status.lines().filter(|l| l.starts_with("??")).count();
    let (ahead, behind) = ahead_behind(cwd);
    Some(crate::session::GitStats {
        files_changed,
        insertions,
        deletions,
        untracked,
        dirty,
        ahead,
        behind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn parse_numstat_sums_changes() {
        let (files, ins, dels) = parse_numstat("1\t2\tfile.rs\n3\t4\tother.rs\n-\t-\tbin.png\n");
        assert_eq!(files, 3);
        assert_eq!(ins, 4);
        assert_eq!(dels, 6);
    }

    #[test]
    fn parse_numstat_empty_is_zero() {
        assert_eq!(parse_numstat(""), (0, 0, 0));
    }

    #[test]
    fn scan_child_repos_finds_git_subdirs_sorted_skipping_others() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Two git repos (.git dir), one plain dir, one hidden git dir.
        for name in ["beta", "alpha"] {
            std::fs::create_dir_all(root.join(name).join(".git")).unwrap();
        }
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::create_dir_all(root.join(".hidden").join(".git")).unwrap();

        let repos = scan_child_repos(root);
        assert_eq!(repos, vec![root.join("alpha"), root.join("beta")]);
    }

    #[test]
    fn scan_child_repos_detects_git_file_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("worktree");
        std::fs::create_dir_all(&wt).unwrap();
        // Worktree checkouts use a `.git` *file*, not a directory.
        std::fs::write(wt.join(".git"), "gitdir: /somewhere\n").unwrap();

        assert!(is_git_repo(&wt));
        assert_eq!(scan_child_repos(root), vec![wt]);
    }

    #[test]
    fn scan_child_repos_missing_parent_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_child_repos(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn create_or_attach_worktree_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("run git")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("file.txt"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let _guard = TestPathGuard::new(tmp.path().join("data"));

        let p1 = create_or_attach_worktree(&repo, "feat/x", &base).expect("first creates");
        assert!(p1.exists());
        let p2 = create_or_attach_worktree(&repo, "feat/x", &base).expect("second reuses");
        assert_eq!(p1, p2);
        // Worktree dir gone but branch remains: re-attach a worktree to it.
        remove_worktree(&repo, &p1).expect("remove worktree");
        let p3 = create_or_attach_worktree(&repo, "feat/x", &base).expect("third reattaches");
        assert_eq!(p1, p3);
        assert!(p3.exists());
    }

    /// Compute the 16-char hex repo hash used in worktree paths.
    fn repo_hash(repo_path: &Path) -> String {
        stable_repo_hash(&repo_path.display().to_string())
    }

    #[test]
    fn worktree_path_simple_branch() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "main").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("main"));
    }

    #[test]
    fn worktree_path_slash_branch() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "feature/foo").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(
            result,
            base.join("worktrees").join(&hash).join("feature-foo")
        );
    }

    #[test]
    fn worktree_path_nested_slashes() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "feature/team/task").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(
            result,
            base.join("worktrees").join(&hash).join("feature-team-task")
        );
    }

    #[test]
    fn worktree_path_no_slashes_unchanged() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "my-branch").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("my-branch"));
    }

    #[test]
    fn worktree_path_trailing_slash() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "branch/").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("branch-"));
    }

    #[test]
    fn worktree_path_leading_slash() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "/branch").unwrap();
        let hash = repo_hash(repo);
        assert_eq!(result, base.join("worktrees").join(&hash).join("-branch"));
    }

    #[test]
    fn worktree_path_different_repos_produce_different_hashes() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let path_a = worktree_path(Path::new("/repo/a"), "main").unwrap();
        let path_b = worktree_path(Path::new("/repo/b"), "main").unwrap();
        assert_ne!(path_a, path_b);
        assert_eq!(path_a.file_name(), path_b.file_name());
    }

    #[test]
    fn worktree_path_same_repo_is_deterministic() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let first = worktree_path(repo, "main").unwrap();
        let second = worktree_path(repo, "main").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn stable_repo_hash_is_pinned_fnv1a() {
        // Pin the exact output so a future swap back to a non-stable hasher
        // (e.g. DefaultHasher/SipHash, whose digest varies across builds) is
        // caught: these are the canonical FNV-1a 64-bit hashes for the inputs.
        assert_eq!(stable_repo_hash(""), "cbf29ce484222325");
        assert_eq!(stable_repo_hash("a"), "af63dc4c8601ec8c");
        assert_eq!(stable_repo_hash("/home/user/repo"), "96e5ae60e8caf52a");
    }

    #[test]
    fn resolve_base_ref_none_without_remote() {
        // A repo with no upstream and no remote refs resolves to no base ref,
        // so sync surfaces an error rather than rebasing onto a missing
        // `origin/main`.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("file.txt"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);

        assert_eq!(resolve_base_ref(repo), None);
    }

    #[test]
    fn resolve_base_ref_prefers_upstream() {
        // A branch tracking an upstream resolves to `@{upstream}`, ahead of the
        // origin/HEAD and origin/main fallbacks.
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        let run = |dir: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Bare remote to push to.
        std::fs::create_dir_all(&remote).unwrap();
        run(&remote, &["init", "-q", "--bare"]);

        // Working repo with one commit, pushed with upstream tracking (`-u`).
        std::fs::create_dir_all(&work).unwrap();
        run(&work, &["init", "-q"]);
        run(&work, &["config", "user.email", "t@example.com"]);
        run(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("file.txt"), "hi").unwrap();
        run(&work, &["add", "."]);
        run(&work, &["commit", "-qm", "init"]);
        run(
            &work,
            &["remote", "add", "origin", &remote.display().to_string()],
        );
        run(&work, &["push", "-q", "-u", "origin", "HEAD"]);

        assert_eq!(resolve_base_ref(&work), Some("@{upstream}".to_string()));
    }

    #[test]
    fn default_branch_prefers_main_over_master() {
        let branches = vec![
            "develop".to_string(),
            "master".to_string(),
            "main".to_string(),
        ];
        // Uses a non-existent path so the git command fails, exercising the fallback.
        let result = default_branch(Path::new("/nonexistent"), &branches);
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn default_branch_falls_back_to_master() {
        let branches = vec!["develop".to_string(), "master".to_string()];
        let result = default_branch(Path::new("/nonexistent"), &branches);
        assert_eq!(result, Some("master".to_string()));
    }

    #[test]
    fn default_branch_returns_none_when_no_candidates() {
        let branches = vec!["develop".to_string(), "feature".to_string()];
        let result = default_branch(Path::new("/nonexistent"), &branches);
        assert_eq!(result, None);
    }

    #[test]
    fn default_branch_returns_none_for_empty_branches() {
        let result = default_branch(Path::new("/nonexistent"), &[]);
        assert_eq!(result, None);
    }

    #[test]
    fn transient_error_detects_could_not_write_index() {
        assert!(is_transient_error("error: could not write index"));
    }

    #[test]
    fn transient_error_detects_unable_to_write_new_index() {
        assert!(is_transient_error("fatal: Unable to write new index file"));
    }

    #[test]
    fn transient_error_detects_index_lock_exists() {
        assert!(is_transient_error(
            "fatal: Unable to create '/repo/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn transient_error_detects_another_git_process() {
        assert!(is_transient_error(
            "Another git process seems to be running in this repository"
        ));
    }

    #[test]
    fn transient_error_rejects_auth_failure() {
        assert!(!is_transient_error(
            "fatal: Authentication failed for 'https://github.com/repo.git'"
        ));
    }

    #[test]
    fn transient_error_rejects_merge_conflict() {
        assert!(!is_transient_error(
            "CONFLICT (content): Merge conflict in src/main.rs"
        ));
    }

    #[test]
    fn transient_error_rejects_empty_string() {
        assert!(!is_transient_error(""));
    }

    #[test]
    fn transient_error_matches_within_anyhow_chain() {
        // is_transient_error is called with format!("{e:#}") which includes anyhow context
        assert!(is_transient_error(
            "git stash failed: could not write index"
        ));
        assert!(is_transient_error(
            "git stash failed: fatal: Unable to create '/repo/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn try_remove_by_age_removes_old_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("index.lock");
        std::fs::write(&lock, "").unwrap();

        // Backdate the file's mtime to exceed STALE_LOCK_AGE
        let old_time = std::time::SystemTime::now() - Duration::from_secs(120);
        let times = std::fs::FileTimes::new().set_modified(old_time);
        let file = std::fs::File::options().write(true).open(&lock).unwrap();
        file.set_times(times).unwrap();

        try_remove_by_age(&lock);
        assert!(!lock.exists(), "old lock should have been removed");
    }

    #[test]
    fn try_remove_by_age_preserves_fresh_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("index.lock");
        std::fs::write(&lock, "").unwrap();

        try_remove_by_age(&lock);
        assert!(lock.exists(), "fresh lock should be preserved");
    }

    // ── remote / ssh helpers ────────────────────────────────────────

    fn host(dest: &str, wt_dir: Option<&str>) -> HostDef {
        HostDef {
            name: "h".into(),
            destination: dest.into(),
            ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
            worktrees_dir: wt_dir.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn program_and_args(cmd: &Command) -> (String, Vec<String>) {
        (
            cmd.get_program().to_string_lossy().into_owned(),
            cmd.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        )
    }

    #[test]
    fn git_command_local_uses_git_with_args() {
        let cmd = git_command(None, Path::new("/repo"), &["branch", "--list"]);
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "git");
        assert_eq!(args, ["branch", "--list"]);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/repo")));
    }

    #[test]
    fn git_command_remote_wraps_in_ssh() {
        let h = host("me@box", None);
        let cmd = git_command(
            Some(&h),
            Path::new("/srv/repo"),
            &["worktree", "add", "-b", "x"],
        );
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "ssh");
        assert_eq!(
            args,
            [
                "-o",
                "ControlMaster=auto",
                "me@box",
                "git",
                "-C",
                "/srv/repo",
                "worktree",
                "add",
                "-b",
                "x",
            ]
        );
        // No local current_dir is set for the remote variant.
        assert_eq!(cmd.get_current_dir(), None);
    }

    #[test]
    fn git_command_wsl_wraps_in_wsl_exe() {
        let h = HostDef::wsl("Ubuntu");
        let cmd = git_command(
            Some(&h),
            Path::new("/home/me/repo"),
            &["worktree", "add", "-b", "x"],
        );
        let (prog, args) = program_and_args(&cmd);
        assert_eq!(prog, "wsl.exe");
        assert_eq!(
            args,
            [
                "-d",
                "Ubuntu",
                "git",
                "-C",
                "/home/me/repo",
                "worktree",
                "add",
                "-b",
                "x",
            ]
        );
        assert_eq!(cmd.get_current_dir(), None);
    }

    #[test]
    fn worktree_path_for_remote_uses_configured_dir() {
        let h = host("me@box", Some("/data/wt"));
        let path = worktree_path_for(Some(&h), Path::new("/srv/repo"), "feature/foo").unwrap();
        let s = path.display().to_string();
        assert!(s.starts_with("/data/wt/"), "got {s}");
        assert!(s.ends_with("/feature-foo"), "got {s}");
    }

    #[test]
    fn worktree_path_for_local_matches_worktree_path() {
        let base = PathBuf::from("/test/data");
        let _guard = TestPathGuard::new(&base);
        let repo = Path::new("/home/user/repo");
        let via_for = worktree_path_for(None, repo, "main").unwrap();
        let direct = worktree_path(repo, "main").unwrap();
        assert_eq!(via_for, direct);
    }

    // ── parse_repo_name_from_url ────────────────────────────────────

    #[test]
    fn parse_ssh_url() {
        assert_eq!(
            parse_repo_name_from_url("git@github.com:user/thurbox.git"),
            Some("thurbox".to_string())
        );
    }

    #[test]
    fn parse_https_url_with_git_suffix() {
        assert_eq!(
            parse_repo_name_from_url("https://github.com/org/api-server.git"),
            Some("api-server".to_string())
        );
    }

    #[test]
    fn parse_https_url_without_git_suffix() {
        assert_eq!(
            parse_repo_name_from_url("https://github.com/org/api-server"),
            Some("api-server".to_string())
        );
    }

    #[test]
    fn parse_empty_url() {
        assert_eq!(parse_repo_name_from_url(""), None);
    }

    #[test]
    fn parse_ssh_url_no_user_path() {
        assert_eq!(
            parse_repo_name_from_url("git@host:repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn parse_url_trailing_slash() {
        // Trailing slash produces empty last segment — rsplit('/').next() = ""
        assert_eq!(
            parse_repo_name_from_url("https://github.com/org/repo/"),
            None
        );
    }
}
