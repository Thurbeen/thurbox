use std::path::{Path, PathBuf};
use std::process::Command;

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
}
