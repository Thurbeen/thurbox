//! The repositories the creation flow remembers, per host, in
//! `bookmarks.json` in the data directory: what the user picked, the folders
//! they imported, and each folder's members as last scanned.
//!
//! Read and rewritten whole, under a lock every process takes ([`crate::paths`]'s
//! file lock), so two interfaces or a CLI never lose each other's change: a list a person
//! builds by hand stays small, and the writers are the command workers and
//! the folder rescan. The newest use leads the list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remembered path on one host (`""` is this machine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub host: String,
    pub repo_path: PathBuf,
    /// Epoch milliseconds of the last pick, which orders the list.
    pub last_used_at: u64,
    /// A folder of repositories, imported whole.
    #[serde(default)]
    pub is_parent: bool,
    /// Whether the path is a git repository; `None` when never established.
    #[serde(default)]
    pub is_git: Option<bool>,
    /// The folder this path was found under by a scan.
    #[serde(default)]
    pub parent_path: Option<PathBuf>,
}

fn file() -> Option<PathBuf> {
    crate::paths::bookmarks_file()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn load() -> Vec<Bookmark> {
    file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn edit<T>(change: impl FnOnce(&mut Vec<Bookmark>) -> T) -> Result<T, String> {
    let path = file().ok_or("could not resolve the data directory")?;
    crate::paths::with_file_lock(&path, || {
        let mut all = load();
        let answer = change(&mut all);
        let text = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;
        crate::paths::write_atomically(&path, text).map_err(|e| format!("write bookmarks: {e}"))?;
        Ok(answer)
    })
    .map_err(|e| format!("lock bookmarks: {e}"))?
}

/// `host`'s bookmarks, the most recently used first. Read without the lock:
/// a write replaces the file whole, so a read sees one version or the other.
pub fn list(host: &str) -> Vec<Bookmark> {
    let mut rows: Vec<Bookmark> = load().into_iter().filter(|b| b.host == host).collect();
    rows.sort_by_key(|b| std::cmp::Reverse(b.last_used_at));
    rows
}

fn upsert(all: &mut Vec<Bookmark>, host: &str, path: &Path) -> usize {
    match all
        .iter()
        .position(|b| b.host == host && b.repo_path == path)
    {
        Some(at) => at,
        None => {
            all.push(Bookmark {
                host: host.to_string(),
                repo_path: path.to_path_buf(),
                last_used_at: 0,
                is_parent: false,
                is_git: None,
                parent_path: None,
            });
            all.len() - 1
        }
    }
}

/// Remember a folder of repositories, or touch it.
pub fn remember_folder(host: &str, path: &Path) -> Result<(), String> {
    edit(|all| {
        let at = upsert(all, host, path);
        all[at].last_used_at = now_ms();
        all[at].is_parent = true;
    })
}

/// Remember a path, or touch it, recording its git-ness when known.
pub fn remember(host: &str, path: &Path, is_git: Option<bool>) -> Result<(), String> {
    edit(|all| {
        let at = upsert(all, host, path);
        all[at].last_used_at = now_ms();
        all[at].is_git = is_git.or(all[at].is_git);
    })
}

/// Replace a remembered folder's members with `children`, a fresh scan. A
/// folder no longer remembered is left forgotten.
pub fn replace_members(host: &str, parent: &Path, children: &[PathBuf]) -> Result<(), String> {
    edit(|all| {
        if !all
            .iter()
            .any(|b| b.host == host && b.repo_path == parent && b.is_parent)
        {
            return;
        }
        all.retain(|b| !(b.host == host && b.parent_path.as_deref() == Some(parent)));
        let now = now_ms();
        for child in children {
            let at = upsert(all, host, child);
            if all[at].last_used_at == 0 {
                all[at].last_used_at = now;
            }
            all[at].is_git = Some(true);
            all[at].parent_path = Some(parent.to_path_buf());
        }
    })
}

/// Forget a path and, for a folder, its members. Whether it was remembered.
pub fn forget(host: &str, path: &Path) -> Result<bool, String> {
    edit(|all| {
        let before = all.len();
        all.retain(|b| !(b.host == host && b.parent_path.as_deref() == Some(path)));
        let members = before - all.len();
        all.retain(|b| !(b.host == host && b.repo_path == path));
        before - members != all.len()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_keeps_its_scanned_members_and_forgetting_it_drops_them() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        let folder = PathBuf::from("/srv/work");
        remember_folder("ssh:box", &folder).unwrap();
        replace_members("ssh:box", &folder, &[folder.join("a"), folder.join("b")]).unwrap();
        remember("", Path::new("/home/me/repo"), Some(true)).unwrap();
        assert_eq!(list("ssh:box").len(), 3);
        assert_eq!(list("").len(), 1);

        replace_members("ssh:box", &folder, &[folder.join("b")]).unwrap();
        assert_eq!(list("ssh:box").len(), 2);

        assert!(forget("ssh:box", &folder).unwrap());
        assert!(list("ssh:box").is_empty());
        assert!(!forget("ssh:box", &folder).unwrap());
    }

    /// Writers that each open the file on their own, as two processes do,
    /// keep every path they remembered.
    #[test]
    fn interleaved_writers_lose_no_bookmark() {
        let dir = tempfile::tempdir().unwrap();
        let writers: Vec<_> = (0..8)
            .map(|writer| {
                let dir = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    let _guard = crate::paths::TestPathGuard::new(&dir);
                    for n in 0..10 {
                        remember("", &PathBuf::from(format!("/{writer}/{n}")), None).unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        assert_eq!(list("").len(), 80);
    }

    #[test]
    fn the_latest_pick_leads_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        remember("", Path::new("/a"), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        remember("", Path::new("/b"), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        remember("", Path::new("/a"), Some(true)).unwrap();
        let rows = list("");
        assert_eq!(rows[0].repo_path, PathBuf::from("/a"));
        assert_eq!(rows[0].is_git, Some(true));
    }
}
