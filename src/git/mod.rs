use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

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
/// Path format: `<repo>/.git/thurbox-worktrees/<sanitized-new-branch>`
pub fn create_worktree(repo_path: &Path, new_branch: &str, base_branch: &str) -> Result<PathBuf> {
    let wt_path = worktree_path(repo_path, new_branch);

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

/// Deterministic worktree directory path for a repo + branch.
fn worktree_path(repo_path: &Path, branch: &str) -> PathBuf {
    let sanitized = branch.replace('/', "-");
    repo_path
        .join(".git")
        .join("thurbox-worktrees")
        .join(sanitized)
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

/// High-level sync: stash, fetch, rebase origin/main, pop stash.
///
/// On conflict the rebase is aborted and any stash is restored.
pub fn sync_worktree(worktree_path: &Path) -> SyncResult {
    let stashed = match git_stash(worktree_path) {
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

    #[test]
    fn worktree_path_simple_branch() {
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "main");
        assert_eq!(
            result,
            PathBuf::from("/home/user/repo/.git/thurbox-worktrees/main")
        );
    }

    #[test]
    fn worktree_path_slash_branch() {
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "feature/foo");
        assert_eq!(
            result,
            PathBuf::from("/home/user/repo/.git/thurbox-worktrees/feature-foo")
        );
    }

    #[test]
    fn worktree_path_nested_slashes() {
        let repo = Path::new("/home/user/repo");
        let result = worktree_path(repo, "feature/team/task");
        assert_eq!(
            result,
            PathBuf::from("/home/user/repo/.git/thurbox-worktrees/feature-team-task")
        );
    }

    #[test]
    fn worktree_path_no_slashes_unchanged() {
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "my-branch");
        assert_eq!(
            result,
            PathBuf::from("/repo/.git/thurbox-worktrees/my-branch")
        );
    }

    #[test]
    fn worktree_path_trailing_slash() {
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "branch/");
        assert_eq!(
            result,
            PathBuf::from("/repo/.git/thurbox-worktrees/branch-")
        );
    }

    #[test]
    fn worktree_path_leading_slash() {
        let repo = Path::new("/repo");
        let result = worktree_path(repo, "/branch");
        assert_eq!(
            result,
            PathBuf::from("/repo/.git/thurbox-worktrees/-branch")
        );
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
}
