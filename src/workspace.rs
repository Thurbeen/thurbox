//! Per-session multi-repo **symlink workspaces**.
//!
//! When a session spans more than one directory (multiple repos, or a repo plus
//! an extra access dir), the agent process can only be launched in a single
//! `cwd`. Rather than teach every agent CLI a different `--add-dir`-style flag
//! (many have none), thurbox builds one workspace directory full of symlinks —
//! one per member dir — and launches the agent there. The agent then sees every
//! repo as a subdirectory, with no per-agent configuration.
//!
//! ```text
//! ~/.local/share/thurbox/workspaces/<agent_session_id>/
//!     webapp  -> …/worktrees/<hash>/feat-x   (symlink)
//!     infra   -> /home/me/repos/infra        (symlink)
//! ```
//!
//! The directory only ever contains symlinks, so tearing it down (or rebuilding
//! it) never touches the underlying repositories. The path is derived from the
//! session's stable `agent_session_id`, so it is rebuilt idempotently on every
//! launch and needs no separate persistence.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths;

/// Resolve the workspace directory for a session id, ensuring it stays a single
/// segment under the workspaces root (defensive — the id is a UUID in practice).
fn workspace_dir(id: &str) -> io::Result<PathBuf> {
    let base = paths::workspaces_directory().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not resolve workspaces directory",
        )
    })?;
    let segment = paths::sanitize_workspace_segment(id);
    if segment.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty workspace id",
        ));
    }
    Ok(base.join(segment))
}

/// (Re)build the symlink workspace for `id` from `members` and return its path.
///
/// Idempotent: any existing workspace dir is removed first (it holds only
/// symlinks, so targets are untouched) and recreated from scratch, so adding or
/// removing a member between launches is reflected. Each member is symlinked
/// under a sanitized, de-duplicated name (collisions get a `-2`, `-3`, … suffix).
pub fn ensure_workspace(id: &str, members: &[(String, PathBuf)]) -> io::Result<PathBuf> {
    let dir = workspace_dir(id)?;
    remove_dir_under_root(&dir)?;
    std::fs::create_dir_all(&dir)?;

    let mut used: HashSet<String> = HashSet::new();
    for (name, target) in members {
        let link_name = paths::unique_link_name(name, &mut used);
        let link_path = dir.join(&link_name);
        symlink(target, &link_path)?;
    }

    Ok(dir)
}

/// The workspace directory path for `id` **without building or touching it** —
/// for callers that need where an existing workspace lives while an agent is
/// still running in it (e.g. the companion shell pane): the destructive
/// rebuild in [`ensure_workspace`] would delete the running agent's cwd inode
/// out from under it.
pub fn workspace_path(id: &str) -> io::Result<PathBuf> {
    workspace_dir(id)
}

/// Remove the workspace directory for `id`, if present. Only the symlinks are
/// removed; the directories they point at are untouched. A missing workspace is
/// not an error.
pub fn remove_workspace(id: &str) -> io::Result<()> {
    let dir = workspace_dir(id)?;
    remove_dir_under_root(&dir)
}

/// Remove `dir` (recursively) only when it really sits under the workspaces
/// root, so a bad id can never delete something outside it. `remove_dir_all`
/// unlinks symlink entries without following them, so member repos are safe.
fn remove_dir_under_root(dir: &Path) -> io::Result<()> {
    let Some(base) = paths::workspaces_directory() else {
        return Ok(());
    };
    if !dir.starts_with(&base) || dir == base {
        return Ok(());
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Create a directory link at `link` pointing to `target`.
///
/// Workspace members are always directories (a worktree checkout or a plain
/// repo dir), so the Unix path uses a plain symlink and the Windows path uses a
/// directory symlink.
#[cfg(not(windows))]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Windows directory link. A directory symlink is preferred, but it requires
/// privilege (Developer Mode or admin) on Windows; when that fails we fall back
/// to an NTFS **junction** (`mklink /J`), which needs no special privilege and
/// also links directories across volumes.
#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    match std::os::windows::fs::symlink_dir(target, link) {
        Ok(()) => Ok(()),
        Err(_) => {
            // `mklink` is a cmd.exe builtin, so it runs via `cmd /C`.
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "mklink /J failed for {} -> {}",
                    link.display(),
                    target.display()
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    fn temp_base() -> PathBuf {
        // Unique-ish per test via the thread name; avoids Date/rand (forbidden).
        let mut p = std::env::temp_dir();
        let t = std::thread::current();
        let name = t.name().unwrap_or("ws").replace("::", "-");
        p.push(format!("thurbox-ws-test-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn ensure_creates_one_symlink_per_member() {
        let base = temp_base();
        let _g = TestPathGuard::new(&base);
        let repo_a = base.join("src-a");
        let repo_b = base.join("src-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();

        let ws = ensure_workspace(
            "sess-1",
            &[
                ("webapp".to_string(), repo_a.clone()),
                ("infra".to_string(), repo_b.clone()),
            ],
        )
        .unwrap();

        assert!(ws.ends_with("workspaces/sess-1"));
        assert_eq!(std::fs::read_link(ws.join("webapp")).unwrap(), repo_a);
        assert_eq!(std::fs::read_link(ws.join("infra")).unwrap(), repo_b);
    }

    #[test]
    fn ensure_is_idempotent_and_reflects_new_members() {
        let base = temp_base();
        let _g = TestPathGuard::new(&base);
        let a = base.join("a");
        let b = base.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        ensure_workspace("s", &[("a".into(), a.clone())]).unwrap();
        let ws =
            ensure_workspace("s", &[("a".into(), a.clone()), ("b".into(), b.clone())]).unwrap();

        assert!(ws.join("a").exists());
        assert!(ws.join("b").exists());
    }

    #[test]
    fn colliding_names_are_disambiguated() {
        let base = temp_base();
        let _g = TestPathGuard::new(&base);
        let a = base.join("one");
        let b = base.join("two");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let ws = ensure_workspace(
            "s",
            &[("repo".into(), a.clone()), ("repo".into(), b.clone())],
        )
        .unwrap();

        assert_eq!(std::fs::read_link(ws.join("repo")).unwrap(), a);
        assert_eq!(std::fs::read_link(ws.join("repo-2")).unwrap(), b);
    }

    #[test]
    fn remove_deletes_links_not_targets() {
        let base = temp_base();
        let _g = TestPathGuard::new(&base);
        let repo = base.join("keepme");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("file.txt"), b"data").unwrap();

        let ws = ensure_workspace("s", &[("repo".into(), repo.clone())]).unwrap();
        assert!(ws.exists());

        remove_workspace("s").unwrap();
        assert!(!ws.exists());
        assert!(repo.join("file.txt").exists());
    }

    #[test]
    fn remove_missing_workspace_is_ok() {
        let base = temp_base();
        let _g = TestPathGuard::new(&base);
        assert!(remove_workspace("never-made").is_ok());
    }
}
