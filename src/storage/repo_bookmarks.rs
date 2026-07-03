use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::sync::current_time_millis;

use super::Database;

/// A bookmarked/recently-used repo path, scoped to the host whose filesystem
/// it lives on (`""` = local, else the backend name `ssh:<name>` /
/// `wsl:<name>`) so a remote target gets its own bookmark memory.
#[derive(Debug, Clone)]
pub struct RepoBookmark {
    pub repo_path: PathBuf,
    pub label: Option<String>,
    pub last_used_at: u64,
    pub use_count: u64,
    /// When true, `repo_path` is a *parent* folder: the repo picker re-scans its
    /// immediate git sub-directories on each open instead of using the path
    /// itself as a repo.
    pub is_parent: bool,
}

impl Database {
    /// List a host's repo bookmarks (`""` = local), sorted by last_used_at
    /// descending (most recent first).
    pub fn list_repo_bookmarks(&self, host: &str) -> rusqlite::Result<Vec<RepoBookmark>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo_path, label, last_used_at, use_count, is_parent \
             FROM repo_bookmarks WHERE host = ?1 ORDER BY last_used_at DESC",
        )?;

        let bookmarks = stmt
            .query_map([host], |row| {
                let path: String = row.get(0)?;
                Ok(RepoBookmark {
                    repo_path: PathBuf::from(path),
                    label: row.get(1)?,
                    last_used_at: row.get::<_, i64>(2)? as u64,
                    use_count: row.get::<_, i64>(3)? as u64,
                    is_parent: row.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(bookmarks)
    }

    /// Add or update a host's repo bookmark (`""` = local). Increments
    /// use_count and updates last_used_at.
    pub fn upsert_repo_bookmark(&self, host: &str, repo_path: &Path) -> rusqlite::Result<()> {
        self.upsert_repo_bookmark_kind(host, repo_path, false)
    }

    /// Add or update a host's repo bookmark, setting whether it is a parent
    /// folder. Increments use_count and updates last_used_at; `is_parent` is
    /// set on both insert and conflict so the kind can be flipped by
    /// re-importing a path.
    pub fn upsert_repo_bookmark_kind(
        &self,
        host: &str,
        repo_path: &Path,
        is_parent: bool,
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let path_str = repo_path.to_string_lossy().to_string();
        self.conn.execute(
            "INSERT INTO repo_bookmarks (host, repo_path, last_used_at, use_count, is_parent) \
             VALUES (?1, ?2, ?3, 1, ?4) \
             ON CONFLICT(host, repo_path) DO UPDATE SET \
                 last_used_at = excluded.last_used_at, \
                 use_count = use_count + 1, \
                 is_parent = excluded.is_parent",
            params![host, path_str, now, is_parent as i64],
        )?;
        Ok(())
    }

    /// Delete a host's repo bookmark. Returns true if it existed.
    pub fn delete_repo_bookmark(&self, host: &str, repo_path: &Path) -> rusqlite::Result<bool> {
        let path_str = repo_path.to_string_lossy().to_string();
        let count = self.conn.execute(
            "DELETE FROM repo_bookmarks WHERE host = ?1 AND repo_path = ?2",
            params![host, path_str],
        )?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_repo_bookmarks_empty() {
        let db = Database::open_in_memory().unwrap();
        let bookmarks = db.list_repo_bookmarks("").unwrap();
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn upsert_and_list_repo_bookmarks() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark("", Path::new("/repo/b")).unwrap();

        let bookmarks = db.list_repo_bookmarks("").unwrap();
        assert_eq!(bookmarks.len(), 2);
        let paths: Vec<&Path> = bookmarks.iter().map(|b| b.repo_path.as_path()).collect();
        assert!(paths.contains(&Path::new("/repo/a")));
        assert!(paths.contains(&Path::new("/repo/b")));
        assert_eq!(bookmarks[0].use_count, 1);
    }

    #[test]
    fn bookmarks_are_scoped_per_host() {
        // The same path on two hosts is two independent bookmarks, and each
        // host's list only shows its own — the point of the (host, repo_path)
        // key: a remote target gets its own memory, never local paths.
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark("ssh:devbox", Path::new("/repo/a"))
            .unwrap();
        db.upsert_repo_bookmark("ssh:devbox", Path::new("/srv/remote"))
            .unwrap();

        let local = db.list_repo_bookmarks("").unwrap();
        assert_eq!(local.len(), 1);
        let remote = db.list_repo_bookmarks("ssh:devbox").unwrap();
        assert_eq!(remote.len(), 2);

        // Deleting on one host leaves the other host's row alone.
        assert!(db
            .delete_repo_bookmark("ssh:devbox", Path::new("/repo/a"))
            .unwrap());
        assert_eq!(db.list_repo_bookmarks("").unwrap().len(), 1);
    }

    #[test]
    fn parent_bookmark_round_trips() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark_kind("", Path::new("/parent/x"), true)
            .unwrap();

        let bookmarks = db.list_repo_bookmarks("").unwrap();
        let parent = bookmarks
            .iter()
            .find(|b| b.repo_path == Path::new("/parent/x"))
            .unwrap();
        assert!(parent.is_parent);
        let repo = bookmarks
            .iter()
            .find(|b| b.repo_path == Path::new("/repo/a"))
            .unwrap();
        assert!(!repo.is_parent);
    }

    #[test]
    fn upsert_increments_use_count() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();

        let bookmarks = db.list_repo_bookmarks("").unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].use_count, 3);
    }

    #[test]
    fn delete_repo_bookmark() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark("", Path::new("/repo/a")).unwrap();
        assert!(db.delete_repo_bookmark("", Path::new("/repo/a")).unwrap());
        assert!(!db.delete_repo_bookmark("", Path::new("/repo/a")).unwrap());
        assert!(db.list_repo_bookmarks("").unwrap().is_empty());
    }
}
