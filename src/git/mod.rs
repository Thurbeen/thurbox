use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::session::{SyncErrorKind, SyncStatus};

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

/// Fetch from a remote repository.
pub fn fetch_remote(repo_path: &Path, remote: &str) -> Result<(), SyncErrorKind> {
    let output = Command::new("git")
        .args(["fetch", remote])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Network)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not resolve host") || stderr.contains("unable to access") {
            return Err(SyncErrorKind::Network);
        }
        return Err(SyncErrorKind::Fetch);
    }

    Ok(())
}

/// Check the sync status of a branch relative to its remote tracking branch.
pub fn sync_status(repo_path: &Path, branch: &str) -> Result<SyncStatus, SyncErrorKind> {
    let remote_ref = format!("origin/{branch}");

    // Check if the remote tracking branch exists
    let check = Command::new("git")
        .args(["rev-parse", "--verify", &remote_ref])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Unknown)?;

    if !check.status.success() {
        // No remote tracking branch — nothing to compare against
        return Ok(SyncStatus::Unknown);
    }

    // Count commits ahead: local has but remote doesn't
    let ahead_output = Command::new("git")
        .args(["rev-list", "--count", &format!("{remote_ref}..HEAD")])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Unknown)?;

    let ahead: usize = String::from_utf8_lossy(&ahead_output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);

    // Count commits behind: remote has but local doesn't
    let behind_output = Command::new("git")
        .args(["rev-list", "--count", &format!("HEAD..{remote_ref}")])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Unknown)?;

    let behind: usize = String::from_utf8_lossy(&behind_output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);

    Ok(match (ahead, behind) {
        (0, 0) => SyncStatus::UpToDate,
        (0, b) => SyncStatus::Behind(b),
        (a, 0) => SyncStatus::Ahead(a),
        (a, b) => SyncStatus::Diverged {
            ahead: a,
            behind: b,
        },
    })
}

/// Pull the latest changes for a branch. Fails if there are local changes.
pub fn pull_branch(repo_path: &Path, branch: &str) -> Result<(), SyncErrorKind> {
    // Check for uncommitted changes first
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Unknown)?;

    let status_text = String::from_utf8_lossy(&status.stdout);
    if !status_text.trim().is_empty() {
        return Err(SyncErrorKind::LocalChanges);
    }

    let output = Command::new("git")
        .args(["pull", "origin", branch])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Network)?;

    if !output.status.success() {
        return Err(SyncErrorKind::Pull);
    }

    Ok(())
}

/// Push the current branch to the remote.
pub fn push_branch(repo_path: &Path, branch: &str) -> Result<(), SyncErrorKind> {
    let output = Command::new("git")
        .args(["push", "origin", branch])
        .current_dir(repo_path)
        .output()
        .map_err(|_| SyncErrorKind::Network)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not resolve host") || stderr.contains("unable to access") {
            return Err(SyncErrorKind::Network);
        }
        return Err(SyncErrorKind::Unknown);
    }

    Ok(())
}

/// Deterministic worktree directory path for a repo + branch.
fn worktree_path(repo_path: &Path, branch: &str) -> PathBuf {
    let sanitized = branch.replace('/', "-");
    repo_path
        .join(".git")
        .join("thurbox-worktrees")
        .join(sanitized)
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
