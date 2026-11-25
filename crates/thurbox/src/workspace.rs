use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths;

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

pub fn workspace_path(id: &str) -> io::Result<PathBuf> {
    workspace_dir(id)
}

pub fn remove_workspace(id: &str) -> io::Result<()> {
    let dir = workspace_dir(id)?;
    remove_dir_under_root(&dir)
}

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

#[cfg(not(windows))]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

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

    fn temp_base() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("thurbox-ws-test-")
            .tempdir()
            .expect("temp base for a workspace test")
    }

    #[test]
    fn the_workspace_test_base_holds_a_workspace() {
        let tmp = temp_base();
        let base = tmp.path();
        let _g = TestPathGuard::new(base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let ws = ensure_workspace("leak-probe", &[("repo".to_string(), repo)]).unwrap();
        assert!(
            ws.starts_with(base),
            "workspace escaped the test base: {}",
            ws.display()
        );
        assert!(
            base.starts_with(std::env::temp_dir()),
            "test base is not under the temp dir: {}",
            base.display()
        );
        println!("TEMP_SANDBOX={}", base.display());
    }

    #[test]
    fn ensure_creates_one_symlink_per_member() {
        let tmp = temp_base();
        let base = tmp.path();
        let _g = TestPathGuard::new(base);
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
        let tmp = temp_base();
        let base = tmp.path();
        let _g = TestPathGuard::new(base);
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
        let tmp = temp_base();
        let base = tmp.path();
        let _g = TestPathGuard::new(base);
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
        let tmp = temp_base();
        let base = tmp.path();
        let _g = TestPathGuard::new(base);
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
        let tmp = temp_base();
        let base = tmp.path();
        let _g = TestPathGuard::new(base);
        assert!(remove_workspace("never-made").is_ok());
    }
}
