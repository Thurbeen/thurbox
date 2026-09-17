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
    /// When true, `repo_path` is a *parent* folder: the repo picker offers its
    /// immediate git sub-directories, re-scanned rather than remembered, instead
    /// of using the path itself as a repo.
    pub is_parent: bool,
    /// Whether the path is a git repo (`None` = never checked). Gates the
    /// picker's worktree toggle; learned opportunistically for remote rows.
    pub is_git: Option<bool>,
    /// Set on a *persisted child* of a remote parent bookmark: the parent folder
    /// it was imported under.
    ///
    /// Remote only, because the two sides persist differently rather than
    /// refresh differently: a local folder is scanned on every read, so there is
    /// nothing worth writing down, while a remote one is scanned on an interval
    /// (`kernel::repos::REMOTE_RESCAN_TTL`) and each scan is written back here —
    /// which is what the folder still shows when the host cannot be reached.
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
    ///
    /// Replace rather than merge, and that is the point: this is called with the
    /// result of a scan, at import and on every re-scan, so a repository deleted
    /// on the host stops being offered instead of outliving it.
    ///
    /// A parent that is **no longer remembered** takes no children: a rescan is
    /// an ssh round trip, so the folder can be forgotten while one is in flight,
    /// and inserting then would file children under a header that no longer
    /// exists — which `flatten` offers as standalone bookmarks, one loose row
    /// per repository the user just forgot. The check is inside the transaction
    /// so a delete committing between it and the insert cannot slip through.
    pub fn replace_parent_children(
        &self,
        host: &str,
        parent: &Path,
        children: &[PathBuf],
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let parent_str = parent.to_string_lossy().to_string();
        // `BEGIN IMMEDIATE` (see `Database::write_transaction`): this reads
        // before it writes, and a deferred transaction that upgrades mid-flight
        // fails with `SQLITE_BUSY` without consulting `busy_timeout`.
        let tx = self.write_transaction()?;
        let remembered = tx.query_row(
            "SELECT 1 FROM repo_bookmarks \
             WHERE host = ?1 AND repo_path = ?2 AND is_parent = 1",
            params![host, parent_str],
            |_| Ok(()),
        );
        match remembered {
            Ok(()) => {}
            // Forgotten while this scan was on its way. Its members went with it
            // (`delete_repo_bookmark` takes them), so there is nothing to clean
            // up and nothing to write.
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
            Err(e) => return Err(e),
        }
        tx.execute(
            "DELETE FROM repo_bookmarks WHERE host = ?1 AND parent_path = ?2",
            params![host, parent_str],
        )?;
        for child in children {
            let child_str = child.to_string_lossy().to_string();
            tx.execute(
                "INSERT INTO repo_bookmarks \
                     (host, repo_path, last_used_at, use_count, is_git, parent_path) \
                 VALUES (?1, ?2, ?3, 1, 1, ?4) \
                 ON CONFLICT(host, repo_path) DO UPDATE SET \
                     is_git = 1, \
                     parent_path = excluded.parent_path",
                params![host, child_str, now, parent_str],
            )?;
        }
        tx.commit()
    }

    /// Delete a host's repo bookmark. Returns true if it existed. Deleting a
    /// parent also drops its persisted children (rows tagged with it via
    /// `parent_path`) so a remote parent group disappears as one unit.
    pub fn delete_repo_bookmark(&self, host: &str, repo_path: &Path) -> rusqlite::Result<bool> {
        let path_str = repo_path.to_string_lossy().to_string();
        // One transaction, because the gap between the two statements is a state
        // no other writer may see: with the members gone and the folder still
        // there, a rescan committing in it passes
        // `replace_parent_children`'s parent check, reinserts its children, and
        // the second DELETE takes only the folder — leaving every member as a
        // bookmark of its own.
        let tx = self.write_transaction()?;
        tx.execute(
            "DELETE FROM repo_bookmarks WHERE host = ?1 AND parent_path = ?2",
            params![host, path_str],
        )?;
        let count = tx.execute(
            "DELETE FROM repo_bookmarks WHERE host = ?1 AND repo_path = ?2",
            params![host, path_str],
        )?;
        tx.commit()?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forgetting a folder must never leave its members behind as loose rows.
    ///
    /// The rescan side of this is staged below; this is the delete side, and it
    /// needs the load shape because the window is *inside* `delete_repo_bookmark`
    /// — between the DELETE of the members and the DELETE of the folder itself.
    /// A rescan committing in that window passes the parent-still-remembered
    /// check, reinserts its children, and the second DELETE takes only the
    /// folder: the members survive with a `parent_path` pointing at nothing, and
    /// `flatten` offers each as a bookmark of its own. Both statements therefore
    /// go in one transaction.
    ///
    /// Weaker than a staged test by design — it can only under-report, since a
    /// scheduler that never interleaves the two simply finds nothing — which is
    /// the same trade `storage::mod`'s continuous-peer test makes, and for the
    /// same reason: there is no hook inside the call to stage against.
    #[test]
    fn forgetting_a_folder_racing_its_rescan_leaves_no_orphans() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        let db = Database::open(&path).unwrap();
        let children: Vec<PathBuf> = (0..8)
            .map(|n| PathBuf::from(format!("/srv/r{n}")))
            .collect();

        for round in 0..200 {
            let folder = PathBuf::from(format!("/srv/f{round}"));
            db.upsert_repo_bookmark_kind("ssh:box", &folder, true)
                .unwrap();
            db.replace_parent_children("ssh:box", &folder, &children)
                .unwrap();

            // The rescan worker and the keypress, on their own connections.
            let scan_path = path.clone();
            let scan_folder = folder.clone();
            let scan_children = children.clone();
            let scan = std::thread::spawn(move || {
                let db = Database::open_existing(&scan_path).unwrap();
                db.replace_parent_children("ssh:box", &scan_folder, &scan_children)
            });
            let forget_path = path.clone();
            let forget_folder = folder.clone();
            let forget = std::thread::spawn(move || {
                let db = Database::open_existing(&forget_path).unwrap();
                db.delete_repo_bookmark("ssh:box", &forget_folder)
            });
            scan.join().unwrap().unwrap();
            forget.join().unwrap().unwrap();

            let left = db.list_repo_bookmarks("ssh:box").unwrap();
            assert!(
                left.is_empty(),
                "round {round}: forgetting the folder left {:?}",
                left.iter().map(|row| &row.repo_path).collect::<Vec<_>>()
            );
        }
    }

    /// A rescan that lands after its folder was forgotten must write nothing.
    ///
    /// Reachable since a remote folder is rescanned in the background: the scan
    /// is an ssh round trip, and `d` on the folder header during it deletes the
    /// parent and its members. The worker then wrote its children back anyway —
    /// and with no parent row left to file them under, `flatten` offers each one
    /// as a standalone bookmark, so forgetting a folder of eight repositories
    /// left eight loose rows to delete by hand.
    #[test]
    fn a_rescan_that_lands_after_its_folder_was_forgotten_writes_nothing() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_repo_bookmark_kind("ssh:box", Path::new("/srv"), true)
            .unwrap();
        db.replace_parent_children("ssh:box", Path::new("/srv"), &[PathBuf::from("/srv/one")])
            .unwrap();

        assert!(db
            .delete_repo_bookmark("ssh:box", Path::new("/srv"))
            .unwrap());
        db.replace_parent_children(
            "ssh:box",
            Path::new("/srv"),
            &[PathBuf::from("/srv/one"), PathBuf::from("/srv/two")],
        )
        .unwrap();

        assert!(
            db.list_repo_bookmarks("ssh:box").unwrap().is_empty(),
            "a forgotten folder's members must not come back as rows of their own"
        );
    }

    /// Two folders rescanning at once must not drop one of the two answers.
    ///
    /// Concurrent callers are new here: this used to run only from the `Alt+P`
    /// import, one keypress at a time, and now also runs from every remote
    /// folder's rescan — so two folder bookmarks on one host come due together
    /// and write from two workers on two connections. What makes that safe is
    /// that the write takes the lock at `BEGIN IMMEDIATE` and so waits out a
    /// peer on `busy_timeout`, instead of upgrading a read snapshot the peer has
    /// superseded — the interleaving WAL refuses outright, whatever the timeout
    /// says (`storage::mod`'s twin of this test carries that rule and the defect
    /// that wrote it). It matters here because the parent-still-remembered check
    /// reads before the DELETE writes.
    ///
    /// Staged rather than raced, for the reason that twin gives: the peer takes
    /// the lock before the write starts and commits only once the call is
    /// proven to be in flight, so there is no interleaving to get lucky about.
    #[test]
    fn replacing_a_folder_s_members_waits_out_a_peer_that_commits_underneath_it() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("thurbox.db");
        let db = Database::open(&path).unwrap();
        db.upsert_repo_bookmark_kind("ssh:box", Path::new("/srv/a"), true)
            .unwrap();
        db.upsert_repo_bookmark_kind("ssh:box", Path::new("/srv/b"), true)
            .unwrap();

        // The peer — the other folder's rescan — holds the write lock and has
        // not committed yet.
        let peer = rusqlite::Connection::open(&path).unwrap();
        peer.busy_timeout(crate::storage::schema::BUSY_TIMEOUT)
            .unwrap();
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let (started, call_started) = mpsc::channel();
        let (done, call_done) = mpsc::channel();
        let write_path = path.clone();
        let writer = std::thread::spawn(move || {
            let db = Database::open_existing(&write_path).unwrap();
            started.send(()).unwrap();
            let outcome = db.replace_parent_children(
                "ssh:box",
                Path::new("/srv/a"),
                &[PathBuf::from("/srv/a/one")],
            );
            let _ = done.send(());
            outcome
        });

        call_started.recv().unwrap();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            assert!(
                call_done.try_recv().is_err(),
                "the write returned while the peer held the lock, so it never blocked"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        peer.execute(
            "UPDATE repo_bookmarks SET use_count = 2 WHERE repo_path = ?1",
            params!["/srv/b"],
        )
        .unwrap();
        peer.execute_batch("COMMIT").unwrap();

        writer
            .join()
            .unwrap()
            .expect("a peer's commit must not fail this rescan");
        let members: Vec<PathBuf> = db
            .list_repo_bookmarks("ssh:box")
            .unwrap()
            .into_iter()
            .filter(|row| row.parent_path.as_deref() == Some(Path::new("/srv/a")))
            .map(|row| row.repo_path)
            .collect();
        assert_eq!(
            members,
            [PathBuf::from("/srv/a/one")],
            "what the scan found is what the folder ends up offering"
        );
    }

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
