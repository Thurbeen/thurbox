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
    /// Whether the path is a git repo (`None` = never checked). Gates the
    /// picker's worktree toggle; learned opportunistically for remote rows.
    pub is_git: Option<bool>,
    /// Set on a *persisted child* of a remote parent bookmark: the parent
    /// folder it was imported under. Local parents scan live instead.
    pub parent_path: Option<PathBuf>,
}

impl Database {
    /// List a host's repo bookmarks (`""` = local), sorted by last_used_at
    /// descending (most recent first).
    pub fn list_repo_bookmarks(&self, host: &str) -> rusqlite::Result<Vec<RepoBookmark>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo_path, label, last_used_at, use_count, is_parent, is_git, parent_path \
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
                    is_git: row.get::<_, Option<i64>>(5)?.map(|v| v != 0),
                    parent_path: row.get::<_, Option<String>>(6)?.map(PathBuf::from),
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

    /// Like [`upsert_repo_bookmark`](Self::upsert_repo_bookmark) but also
    /// records git-ness. `None` never downgrades a known value (COALESCE keeps
    /// the existing one), so a caller that didn't check doesn't erase a caller
    /// that did.
    pub fn upsert_repo_bookmark_checked(
        &self,
        host: &str,
        repo_path: &Path,
        is_git: Option<bool>,
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let path_str = repo_path.to_string_lossy().to_string();
        self.conn.execute(
            "INSERT INTO repo_bookmarks (host, repo_path, last_used_at, use_count, is_git) \
             VALUES (?1, ?2, ?3, 1, ?4) \
             ON CONFLICT(host, repo_path) DO UPDATE SET \
                 last_used_at = excluded.last_used_at, \
                 use_count = use_count + 1, \
                 is_git = COALESCE(excluded.is_git, is_git)",
            params![host, path_str, now, is_git.map(|g| g as i64)],
        )?;
        Ok(())
    }

    /// Record a bookmark's git-ness without touching its recency (unlike the
    /// upserts, which bump `last_used_at`/`use_count`). Used by the picker's
    /// open-time backfill of legacy rows. A missing row is a no-op.
    pub fn set_bookmark_git_kind(
        &self,
        host: &str,
        repo_path: &Path,
        is_git: bool,
    ) -> rusqlite::Result<()> {
        let path_str = repo_path.to_string_lossy().to_string();
        self.conn.execute(
            "UPDATE repo_bookmarks SET is_git = ?3 \
             WHERE host = ?1 AND repo_path = ?2",
            params![host, path_str, is_git as i64],
        )?;
        Ok(())
    }

    /// Replace the persisted children of a remote parent bookmark: delete every
    /// row filed under `parent`, then insert `children` as git repos tagged with
    /// it. Transactional so a failed insert never leaves the parent half-empty.
    pub fn replace_parent_children(
        &self,
        host: &str,
        parent: &Path,
        children: &[PathBuf],
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let parent_str = parent.to_string_lossy().to_string();
        self.conn.execute_batch("BEGIN")?;
        let result = (|| {
            self.conn.execute(
                "DELETE FROM repo_bookmarks WHERE host = ?1 AND parent_path = ?2",
                params![host, parent_str],
            )?;
            for child in children {
                let child_str = child.to_string_lossy().to_string();
                self.conn.execute(
                    "INSERT INTO repo_bookmarks \
                         (host, repo_path, last_used_at, use_count, is_git, parent_path) \
                     VALUES (?1, ?2, ?3, 1, 1, ?4) \
                     ON CONFLICT(host, repo_path) DO UPDATE SET \
                         is_git = 1, \
                         parent_path = excluded.parent_path",
                    params![host, child_str, now, parent_str],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(_) => self.conn.execute_batch("ROLLBACK")?,
        }
        result
    }

    /// Delete a host's repo bookmark. Returns true if it existed. Deleting a
    /// parent also drops its persisted children (rows tagged with it via
    /// `parent_path`) so a remote parent group disappears as one unit.
    pub fn delete_repo_bookmark(&self, host: &str, repo_path: &Path) -> rusqlite::Result<bool> {
        let path_str = repo_path.to_string_lossy().to_string();
        self.conn.execute(
            "DELETE FROM repo_bookmarks WHERE host = ?1 AND parent_path = ?2",
            params![host, path_str],
        )?;
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
        assert_eq!(bookmarks[0].is_git, None);
        assert_eq!(bookmarks[0].parent_path, None);
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

    #[test]
    fn checked_upsert_records_and_never_downgrades_git_kind() {
        let db = Database::open_in_memory().unwrap();

        // Unknown stays unknown.
        db.upsert_repo_bookmark_checked("", Path::new("/repo/a"), None)
            .unwrap();
        assert_eq!(db.list_repo_bookmarks("").unwrap()[0].is_git, None);

        // A check upgrades it…
        db.upsert_repo_bookmark_checked("", Path::new("/repo/a"), Some(true))
            .unwrap();
        assert_eq!(db.list_repo_bookmarks("").unwrap()[0].is_git, Some(true));

        // …and a later unchecked touch must not erase what we learned.
        db.upsert_repo_bookmark_checked("", Path::new("/repo/a"), None)
            .unwrap();
        let row = &db.list_repo_bookmarks("").unwrap()[0];
        assert_eq!(row.is_git, Some(true));
        assert_eq!(row.use_count, 3);

        // A known non-repo round-trips as Some(false).
        db.upsert_repo_bookmark_checked("", Path::new("/plain/dir"), Some(false))
            .unwrap();
        let plain = db
            .list_repo_bookmarks("")
            .unwrap()
            .into_iter()
            .find(|b| b.repo_path == Path::new("/plain/dir"))
            .unwrap();
        assert_eq!(plain.is_git, Some(false));
    }

    #[test]
    fn replace_parent_children_replaces_atomically() {
        let db = Database::open_in_memory().unwrap();
        let host = "ssh:devbox";
        let parent = Path::new("/srv/projects");

        db.upsert_repo_bookmark_kind(host, parent, true).unwrap();
        db.replace_parent_children(
            host,
            parent,
            &[
                PathBuf::from("/srv/projects/a"),
                PathBuf::from("/srv/projects/b"),
            ],
        )
        .unwrap();

        let children = |db: &Database| {
            let mut c: Vec<PathBuf> = db
                .list_repo_bookmarks(host)
                .unwrap()
                .into_iter()
                .filter(|b| b.parent_path.as_deref() == Some(parent))
                .map(|b| b.repo_path)
                .collect();
            c.sort();
            c
        };
        assert_eq!(
            children(&db),
            vec![
                PathBuf::from("/srv/projects/a"),
                PathBuf::from("/srv/projects/b")
            ]
        );
        assert!(children(&db).iter().all(|p| db
            .list_repo_bookmarks(host)
            .unwrap()
            .iter()
            .any(|b| &b.repo_path == p && b.is_git == Some(true))));

        // A re-import replaces the set: dropped children disappear, new ones land.
        db.replace_parent_children(host, parent, &[PathBuf::from("/srv/projects/c")])
            .unwrap();
        assert_eq!(children(&db), vec![PathBuf::from("/srv/projects/c")]);
    }

    #[test]
    fn deleting_parent_cascades_persisted_children() {
        let db = Database::open_in_memory().unwrap();
        let host = "ssh:devbox";
        let parent = Path::new("/srv/projects");

        db.upsert_repo_bookmark_kind(host, parent, true).unwrap();
        db.replace_parent_children(host, parent, &[PathBuf::from("/srv/projects/a")])
            .unwrap();
        // A standalone bookmark not under the parent must survive the cascade.
        db.upsert_repo_bookmark(host, Path::new("/srv/other"))
            .unwrap();

        assert!(db.delete_repo_bookmark(host, parent).unwrap());
        let remaining = db.list_repo_bookmarks(host).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].repo_path, PathBuf::from("/srv/other"));
    }
}
