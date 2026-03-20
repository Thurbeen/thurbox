use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::sync::current_time_millis;

use super::Database;

/// A bookmarked/recently-used repo path.
#[derive(Debug, Clone)]
pub struct RepoBookmark {
    pub repo_path: PathBuf,
    pub label: Option<String>,
    pub last_used_at: u64,
    pub use_count: u64,
}

impl Database {
    /// List all repo bookmarks, sorted by last_used_at descending (most recent first).
    pub fn list_repo_bookmarks(&self) -> rusqlite::Result<Vec<RepoBookmark>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo_path, label, last_used_at, use_count \
             FROM repo_bookmarks ORDER BY last_used_at DESC",
        )?;

        let bookmarks = stmt
            .query_map([], |row| {
                let path: String = row.get(0)?;
                Ok(RepoBookmark {
                    repo_path: PathBuf::from(path),
                    label: row.get(1)?,
                    last_used_at: row.get::<_, i64>(2)? as u64,
                    use_count: row.get::<_, i64>(3)? as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(bookmarks)
    }

    /// Add or update a repo bookmark. Increments use_count and updates last_used_at.
    pub fn upsert_repo_bookmark(&self, repo_path: &Path) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let path_str = repo_path.to_string_lossy().to_string();
        self.conn.execute(
            "INSERT INTO repo_bookmarks (repo_path, last_used_at, use_count) \
             VALUES (?1, ?2, 1) \
             ON CONFLICT(repo_path) DO UPDATE SET \
                 last_used_at = excluded.last_used_at, \
                 use_count = use_count + 1",
            params![path_str, now],
        )?;
        Ok(())
    }

    /// Touch a bookmark (update last_used_at) without incrementing use_count.
    pub fn touch_repo_bookmark(&self, repo_path: &Path) -> rusqlite::Result<bool> {
        let now = current_time_millis() as i64;
        let path_str = repo_path.to_string_lossy().to_string();
        let count = self.conn.execute(
            "UPDATE repo_bookmarks SET last_used_at = ?1 WHERE repo_path = ?2",
            params![now, path_str],
        )?;
        Ok(count > 0)
    }

    /// Delete a repo bookmark. Returns true if it existed.
    pub fn delete_repo_bookmark(&self, repo_path: &Path) -> rusqlite::Result<bool> {
        let path_str = repo_path.to_string_lossy().to_string();
        let count = self.conn.execute(
            "DELETE FROM repo_bookmarks WHERE repo_path = ?1",
            params![path_str],
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
        let bookmarks = db.list_repo_bookmarks().unwrap();
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn upsert_and_list_repo_bookmarks() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark(Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark(Path::new("/repo/b")).unwrap();

        let bookmarks = db.list_repo_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 2);
        let paths: Vec<&Path> = bookmarks.iter().map(|b| b.repo_path.as_path()).collect();
        assert!(paths.contains(&Path::new("/repo/a")));
        assert!(paths.contains(&Path::new("/repo/b")));
        assert_eq!(bookmarks[0].use_count, 1);
    }

    #[test]
    fn upsert_increments_use_count() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark(Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark(Path::new("/repo/a")).unwrap();
        db.upsert_repo_bookmark(Path::new("/repo/a")).unwrap();

        let bookmarks = db.list_repo_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].use_count, 3);
    }

    #[test]
    fn delete_repo_bookmark() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark(Path::new("/repo/a")).unwrap();
        assert!(db.delete_repo_bookmark(Path::new("/repo/a")).unwrap());
        assert!(!db.delete_repo_bookmark(Path::new("/repo/a")).unwrap());
        assert!(db.list_repo_bookmarks().unwrap().is_empty());
    }

    #[test]
    fn touch_updates_last_used_at() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_repo_bookmark(Path::new("/repo/a")).unwrap();
        let before = db.list_repo_bookmarks().unwrap()[0].use_count;
        assert!(db.touch_repo_bookmark(Path::new("/repo/a")).unwrap());
        let after = db.list_repo_bookmarks().unwrap()[0].use_count;
        // touch doesn't increment use_count
        assert_eq!(before, after);
    }

    #[test]
    fn touch_nonexistent_returns_false() {
        let db = Database::open_in_memory().unwrap();
        assert!(!db.touch_repo_bookmark(Path::new("/nope")).unwrap());
    }
}
