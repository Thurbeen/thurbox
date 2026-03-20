use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

use crate::paths;

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
    let output = Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(repo_path)
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

/// Create a git worktree on a new branch and return the worktree directory path.
///
/// Creates `new_branch` starting from `base_branch`.
/// Path format: `~/.local/share/thurbox/worktrees/<repo-hash>/<sanitized-branch>`
pub fn create_worktree(repo_path: &Path, new_branch: &str, base_branch: &str) -> Result<PathBuf> {
    let wt_path =
        worktree_path(repo_path, new_branch).context("failed to resolve worktrees directory")?;

    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            new_branch,
            &wt_path.display().to_string(),
            base_branch,
        ])
        .current_dir(repo_path)
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {stderr}");
    }

    Ok(wt_path)
}

/// Remove a git worktree (force removal).
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &worktree_path.display().to_string(),
        ])
        .current_dir(repo_path)
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
    if let Some(name) = default_branch_from_remote(repo_path) {
        if local_branches.iter().any(|b| b == &name) {
            return Some(name);
        }
    }

    // Fallback: prefer "main", then "master"
    for candidate in ["main", "master"] {
        if local_branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

/// Query the remote's default branch via `git symbolic-ref`.
fn default_branch_from_remote(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
        .current_dir(repo_path)
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
    Command::new("git")
        .args(["rev-parse", "--verify", branch])
        .current_dir(repo_path)
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
    let base = paths::worktrees_directory()?;
    let mut hasher = DefaultHasher::new();
    repo_path.display().to_string().hash(&mut hasher);
    let repo_hash = format!("{:016x}", hasher.finish());
    let sanitized = branch.replace('/', "-");
    Some(base.join(repo_hash).join(sanitized))
}

/// Result of attempting to sync a worktree with origin/main.
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
    // "No local changes to save" means nothing was stashed
    Ok(!stdout.contains("No local changes to save"))
}

/// Fetch from origin.
fn git_fetch(worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(worktree_path)
        .output()
        .context("failed to run git fetch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git fetch failed: {stderr}");
    }

    Ok(())
}

/// Rebase current branch onto origin/main. Returns `Ok(())` on success,
/// or an error if there are conflicts (rebase is aborted before returning).
fn git_rebase_main(worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["rebase", "origin/main"])
        .current_dir(worktree_path)
        .output()
        .context("failed to run git rebase")?;

    if !output.status.success() {
        // Abort the failed rebase
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
        return true; // process still alive
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

/// High-level sync: stash, fetch, rebase origin/main, pop stash.
///
/// On conflict the rebase is aborted and any stash is restored.
/// Retries `git stash` on transient index-lock errors.
pub fn sync_worktree(worktree_path: &Path) -> SyncResult {
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

    if let Err(e) = git_rebase_main(worktree_path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    /// Compute the 16-char hex repo hash used in worktree paths.
    fn repo_hash(repo_path: &Path) -> String {
        let mut hasher = DefaultHasher::new();
        repo_path.display().to_string().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
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
        // Both end with the same branch name
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
