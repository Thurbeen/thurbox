//! The one-time repair of rows the WSL loopback bug recorded as remote.
//!
//! Two halves live apart on purpose. Schema v47 only *marks* the repair as
//! owed, because which backend names to heal is decided by the host registry
//! (`hosts.toml` + WSL discovery) and `storage` may not read it; the set is
//! computed by `agent::host_config::wsl_loopback_backend_names` and handed to
//! [`Database::relabel_wsl_loopback_rows`] by
//! `session_ops::repair_wsl_loopback_rows`. What stays here is the SQL, which
//! is this module's job whoever decides the policy.

use rusqlite::{params, OptionalExtension};

use super::Database;

/// Metadata key set by schema v47 to record that the loopback repair is owed,
/// and cleared once it has run.
pub(super) const WSL_LOOPBACK_REPAIR_OWED_KEY: &str = "wsl_loopback_repair_owed";

/// What one repair pass changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WslLoopbackRepair {
    /// Sessions moved from a loopback backend name back to local.
    pub sessions: usize,
    /// Bookmarks relabelled from a loopback host to local.
    pub bookmarks: usize,
    /// Bookmarks deleted because another reading of the same path won on
    /// recency — `(host, repo_path)` is the key, so only one can survive.
    pub bookmarks_superseded: usize,
}

impl WslLoopbackRepair {
    /// Whether the pass changed anything worth telling the user about.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One `repo_bookmarks` row, as the collision resolution below reads it.
struct Bookmark {
    host: String,
    repo_path: String,
    last_used_at: i64,
}

impl Bookmark {
    fn is_local(&self) -> bool {
        self.host.is_empty()
    }

    /// Descending sort key: most recent first, a tie kept by the local row,
    /// and a tie between two loopback spellings broken by name so the outcome
    /// does not depend on row order.
    fn precedence(&self) -> (i64, bool, std::cmp::Reverse<&str>) {
        (
            self.last_used_at,
            self.is_local(),
            std::cmp::Reverse(self.host.as_str()),
        )
    }
}

impl Database {
    /// Whether the WSL-loopback repair schema v47 recorded is still owed.
    pub fn wsl_loopback_repair_owed(&self) -> rusqlite::Result<bool> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![WSL_LOOPBACK_REPAIR_OWED_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(raw.as_deref() == Some("1"))
    }

    /// Clear the mark, so the repair runs once per database rather than once
    /// per start.
    pub fn clear_wsl_loopback_repair_owed(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM metadata WHERE key = ?1",
            params![WSL_LOOPBACK_REPAIR_OWED_KEY],
        )?;
        Ok(())
    }

    /// Relabel every row recorded under one of `heal` as local: a session's
    /// `backend_type` to [`crate::session::LOCAL_BACKEND_TYPE`], a bookmark's
    /// `host` to `''`.
    ///
    /// Matching is case-insensitive, the way `wsl.exe -d` matches a distro
    /// name, so several spellings of one distro heal together. That is also
    /// why the bookmark half cannot be a bare `UPDATE`: `(host, repo_path)` is
    /// the primary key and it is BINARY, so `wsl:Ubuntu`, `wsl:ubuntu` and a
    /// pre-bug local row can all hold the same `repo_path` and the relabel
    /// would collide. Every such group is one path used locally, so it is
    /// resolved on recency — most recent reading wins, a tie keeps the local
    /// row — and the losers are deleted before the survivor is relabelled.
    /// A `use_count` merge was considered and rejected: this is an MRU hint.
    pub fn relabel_wsl_loopback_rows(
        &self,
        heal: &[String],
    ) -> rusqlite::Result<WslLoopbackRepair> {
        let mut report = WslLoopbackRepair::default();
        if heal.is_empty() {
            return Ok(report);
        }
        let matches_heal = |host: &str| {
            !host.is_empty() && heal.iter().any(|name| name.eq_ignore_ascii_case(host))
        };

        let tx = self.write_transaction()?;

        for name in heal {
            report.sessions += tx.execute(
                "UPDATE sessions SET backend_type = ?1 WHERE backend_type = ?2 COLLATE NOCASE",
                params![crate::session::LOCAL_BACKEND_TYPE, name],
            )?;
        }

        let rows: Vec<Bookmark> = tx
            .prepare("SELECT host, repo_path, last_used_at FROM repo_bookmarks")?
            .query_map([], |row| {
                Ok(Bookmark {
                    host: row.get(0)?,
                    repo_path: row.get(1)?,
                    last_used_at: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut by_path: std::collections::BTreeMap<&str, Vec<&Bookmark>> =
            std::collections::BTreeMap::new();
        for row in &rows {
            if row.is_local() || matches_heal(&row.host) {
                by_path.entry(&row.repo_path).or_default().push(row);
            }
        }

        for (repo_path, mut group) in by_path {
            if !group.iter().any(|b| matches_heal(&b.host)) {
                continue;
            }
            group.sort_by(|a, b| b.precedence().cmp(&a.precedence()));
            let (winner, superseded) = group.split_first().expect("group is never empty");
            for loser in superseded {
                tx.execute(
                    "DELETE FROM repo_bookmarks WHERE host = ?1 AND repo_path = ?2",
                    params![loser.host, repo_path],
                )?;
                report.bookmarks_superseded += 1;
            }
            if !winner.is_local() {
                tx.execute(
                    "UPDATE repo_bookmarks SET host = '' WHERE host = ?1 AND repo_path = ?2",
                    params![winner.host, repo_path],
                )?;
                report.bookmarks += 1;
            }
        }

        tx.commit()?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn bookmark(db: &Database, host: &str, repo_path: &str, label: &str, last_used_at: i64) {
        db.conn
            .execute(
                "INSERT INTO repo_bookmarks (host, repo_path, label, last_used_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![host, repo_path, label, last_used_at],
            )
            .unwrap();
    }

    fn session(db: &Database, id: &str, backend_type: &str) {
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, agent, backend_type, backend_id, \
                 created_at, updated_at) VALUES (?1, ?1, 'claude', ?2, '%1', 0, 0)",
                params![id, backend_type],
            )
            .unwrap();
    }

    fn backends(db: &Database) -> Vec<(String, String)> {
        db.conn
            .prepare("SELECT id, backend_type FROM sessions ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn bookmarks(db: &Database) -> Vec<(String, String, String)> {
        db.conn
            .prepare("SELECT host, repo_path, label FROM repo_bookmarks ORDER BY repo_path, host")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn only_the_named_backends_become_local() {
        let db = db();
        session(&db, "a", "wsl:MagicDebian");
        session(&db, "b", "wsl:magicdebian");
        session(&db, "c", "wsl:MagicDebianPerso");
        session(&db, "d", "ssh:devbox");
        session(&db, "e", "local-tmux");

        let report = db
            .relabel_wsl_loopback_rows(&["wsl:MagicDebian".to_string()])
            .unwrap();

        assert_eq!(report.sessions, 2, "a spelling variant is the same distro");
        assert_eq!(
            backends(&db),
            vec![
                ("a".into(), "local-tmux".into()),
                ("b".into(), "local-tmux".into()),
                ("c".into(), "wsl:MagicDebianPerso".into()),
                ("d".into(), "ssh:devbox".into()),
                ("e".into(), "local-tmux".into()),
            ]
        );
    }

    #[test]
    fn an_empty_set_changes_nothing() {
        let db = db();
        session(&db, "a", "wsl:MagicDebian");
        bookmark(&db, "wsl:MagicDebian", "/repo", "keep", 1);

        assert!(db.relabel_wsl_loopback_rows(&[]).unwrap().is_empty());

        assert_eq!(backends(&db), vec![("a".into(), "wsl:MagicDebian".into())]);
        assert_eq!(
            bookmarks(&db),
            vec![("wsl:MagicDebian".into(), "/repo".into(), "keep".into())]
        );
    }

    /// `(host, repo_path)` is the bookmark key, so a path bookmarked both
    /// before the bug and during it collides on the relabel. The pair is one
    /// path used locally twice, so the more recent reading survives — and a
    /// tie keeps the local row rather than the artifact of the bug.
    #[test]
    fn colliding_bookmarks_resolve_on_recency() {
        let db = db();
        bookmark(&db, "", "/local-newer", "keep", 200);
        bookmark(&db, "wsl:MagicDebian", "/local-newer", "drop", 100);
        bookmark(&db, "", "/loopback-newer", "drop", 100);
        bookmark(&db, "wsl:MagicDebian", "/loopback-newer", "keep", 200);
        bookmark(&db, "", "/tie", "keep", 300);
        bookmark(&db, "wsl:MagicDebian", "/tie", "drop", 300);
        bookmark(&db, "wsl:MagicDebian", "/only-loopback", "keep", 100);
        bookmark(&db, "", "/untouched", "keep", 100);
        bookmark(&db, "ssh:devbox", "/local-newer", "keep", 100);

        let report = db
            .relabel_wsl_loopback_rows(&["wsl:MagicDebian".to_string()])
            .unwrap();
        assert_eq!(report.bookmarks, 2);
        assert_eq!(report.bookmarks_superseded, 3);

        assert_eq!(
            bookmarks(&db),
            vec![
                ("".into(), "/local-newer".into(), "keep".into()),
                // A genuinely remote bookmark for the same path is a different
                // key and never part of the collision.
                ("ssh:devbox".into(), "/local-newer".into(), "keep".into()),
                ("".into(), "/loopback-newer".into(), "keep".into()),
                ("".into(), "/only-loopback".into(), "keep".into()),
                ("".into(), "/tie".into(), "keep".into()),
                ("".into(), "/untouched".into(), "keep".into()),
            ]
        );
    }

    /// The failure mode this must rule out: two case-variant loopback
    /// spellings bookmarking one repo both relabel to `host = ''`, which is
    /// one BINARY primary key for two rows. Raising SQLITE_CONSTRAINT here
    /// would repeat on every start.
    #[test]
    fn two_loopback_spellings_of_one_repo_do_not_violate_the_key() {
        let db = db();
        bookmark(&db, "wsl:Ubuntu", "/repo", "older", 100);
        bookmark(&db, "wsl:ubuntu", "/repo", "newer", 200);
        bookmark(&db, "wsl:UBUNTU", "/other", "only", 100);

        let report = db
            .relabel_wsl_loopback_rows(&["wsl:Ubuntu".to_string(), "wsl:ubuntu".to_string()])
            .unwrap();
        assert_eq!(report.bookmarks, 2);
        assert_eq!(report.bookmarks_superseded, 1);

        assert_eq!(
            bookmarks(&db),
            vec![
                ("".into(), "/other".into(), "only".into()),
                ("".into(), "/repo".into(), "newer".into()),
            ]
        );
    }

    #[test]
    fn the_owed_mark_is_read_and_cleared() {
        let db = db();
        assert!(!db.wsl_loopback_repair_owed().unwrap());
        db.conn
            .execute(
                "INSERT INTO metadata (key, value) VALUES (?1, '1')",
                params![WSL_LOOPBACK_REPAIR_OWED_KEY],
            )
            .unwrap();
        assert!(db.wsl_loopback_repair_owed().unwrap());
        db.clear_wsl_loopback_repair_owed().unwrap();
        assert!(!db.wsl_loopback_repair_owed().unwrap());
    }
}
