//! The one-time repair of rows a WSL loopback or shadow host recorded wrong.
//!
//! Two halves live apart on purpose. Schema v47 only *marks* the repair as
//! owed, because what to rewrite is decided by the host registry — which entry
//! claims `wsl:<us>`, and which distro it really reaches — and `storage` may
//! not read `hosts.toml`. The plan is built by
//! `agent::host_config::wsl_repair_plan` and handed to
//! [`Database::apply_wsl_repair_plan`] by
//! `session_ops::repair_wsl_loopback_rows`. What stays here is the SQL, which
//! is this module's job whoever decides the policy.

use rusqlite::{params, OptionalExtension};

use crate::session::WslRepairPlan;

use super::Database;

/// Metadata key set by schema v47 to record that the repair is owed, and
/// cleared once it has run.
pub(super) const WSL_LOOPBACK_REPAIR_OWED_KEY: &str = "wsl_loopback_repair_owed";

/// What one repair pass changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WslLoopbackRepair {
    /// Sessions restored as this machine's own.
    pub sessions_local: usize,
    /// Bookmarks restored as this machine's own.
    pub bookmarks_local: usize,
    /// Sessions moved onto the host that actually reaches their distro.
    pub sessions_moved: usize,
    /// Bookmarks moved onto that host.
    pub bookmarks_moved: usize,
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

/// A `repo_bookmarks` row in one move's collision group, as the resolution
/// below reads it.
struct Candidate<'a> {
    host: &'a str,
    last_used_at: i64,
    /// Whether this row already carries the host the move writes, so it needs
    /// no rewrite and wins a tie.
    at_destination: bool,
}

impl Candidate<'_> {
    /// Descending sort key: most recent first, a tie kept by the row already
    /// at the destination, and a tie between two spellings of the source
    /// broken by name so the outcome does not depend on row order.
    fn precedence(&self) -> (i64, bool, std::cmp::Reverse<&str>) {
        (
            self.last_used_at,
            self.at_destination,
            std::cmp::Reverse(self.host),
        )
    }
}

/// Move every row recorded under `from` (matched case-insensitively, the way
/// `wsl.exe -d` matches a distro name, so several spellings of one distro move
/// together) to `session_to` / `bookmark_to`.
///
/// The bookmark half cannot be a bare `UPDATE`: `(host, repo_path)` is the
/// primary key and it is BINARY, so `wsl:Ubuntu`, `wsl:ubuntu` and a row
/// already at the destination can all hold the same `repo_path`, and the
/// rewrite would collide. Every such group is one path reached one way, so it
/// is resolved on recency — most recent reading wins, a tie keeps the row
/// already at the destination — and the losers are deleted before the survivor
/// is rewritten. A `use_count` merge was considered and rejected: this is an
/// MRU hint.
///
/// Returns `(sessions, bookmarks, bookmarks_superseded)`.
fn move_rows(
    tx: &rusqlite::Transaction<'_>,
    from: &str,
    session_to: &str,
    bookmark_to: &str,
) -> rusqlite::Result<(usize, usize, usize)> {
    let sessions = tx.execute(
        "UPDATE sessions SET backend_type = ?1 WHERE backend_type = ?2 COLLATE NOCASE",
        params![session_to, from],
    )?;

    let rows: Vec<(String, String, i64)> = tx
        .prepare("SELECT host, repo_path, last_used_at FROM repo_bookmarks")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut by_path: std::collections::BTreeMap<&str, Vec<Candidate>> =
        std::collections::BTreeMap::new();
    for (host, repo_path, last_used_at) in &rows {
        let at_destination = host == bookmark_to;
        if at_destination || host.eq_ignore_ascii_case(from) {
            by_path.entry(repo_path).or_default().push(Candidate {
                host,
                last_used_at: *last_used_at,
                at_destination,
            });
        }
    }

    let mut moved = 0;
    let mut superseded = 0;
    for (repo_path, mut group) in by_path {
        if !group.iter().any(|c| !c.at_destination) {
            continue;
        }
        group.sort_by(|a, b| b.precedence().cmp(&a.precedence()));
        let (winner, losers) = group.split_first().expect("group is never empty");
        for loser in losers {
            tx.execute(
                "DELETE FROM repo_bookmarks WHERE host = ?1 AND repo_path = ?2",
                params![loser.host, repo_path],
            )?;
            superseded += 1;
        }
        if !winner.at_destination {
            tx.execute(
                "UPDATE repo_bookmarks SET host = ?1 WHERE host = ?2 AND repo_path = ?3",
                params![bookmark_to, winner.host, repo_path],
            )?;
            moved += 1;
        }
    }
    Ok((sessions, moved, superseded))
}

impl Database {
    /// Whether the WSL repair schema v47 recorded is still owed.
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

    /// Apply `plan`: rows recorded under a
    /// [`to_local`](WslRepairPlan::to_local) name become this machine's own
    /// (`backend_type` = [`crate::session::LOCAL_BACKEND_TYPE`], bookmark
    /// `host` = `''`), and each [`rename`](WslRepairPlan::renames) moves its
    /// rows onto another host's backend name.
    ///
    /// One transaction, and `to_local` first — the plan's own ordering rule,
    /// which is what stops a renamed row being carried straight on to local
    /// when a loopback happens to be named after the rename's destination.
    pub fn apply_wsl_repair_plan(
        &self,
        plan: &WslRepairPlan,
    ) -> rusqlite::Result<WslLoopbackRepair> {
        let mut report = WslLoopbackRepair::default();
        if plan.is_empty() {
            return Ok(report);
        }
        let tx = self.write_transaction()?;

        for from in &plan.to_local {
            let (sessions, bookmarks, superseded) =
                move_rows(&tx, from, crate::session::LOCAL_BACKEND_TYPE, "")?;
            report.sessions_local += sessions;
            report.bookmarks_local += bookmarks;
            report.bookmarks_superseded += superseded;
        }
        for (from, to) in &plan.renames {
            let (sessions, bookmarks, superseded) = move_rows(&tx, from, to, to)?;
            report.sessions_moved += sessions;
            report.bookmarks_moved += bookmarks;
            report.bookmarks_superseded += superseded;
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

    fn to_local(names: &[&str]) -> WslRepairPlan {
        WslRepairPlan {
            to_local: names.iter().map(|n| n.to_string()).collect(),
            renames: Vec::new(),
        }
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
            .apply_wsl_repair_plan(&to_local(&["wsl:MagicDebian"]))
            .unwrap();

        assert_eq!(
            report.sessions_local, 2,
            "a spelling variant is the same distro"
        );
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
    fn an_empty_plan_changes_nothing() {
        let db = db();
        session(&db, "a", "wsl:MagicDebian");
        bookmark(&db, "wsl:MagicDebian", "/repo", "keep", 1);

        assert!(db
            .apply_wsl_repair_plan(&WslRepairPlan::default())
            .unwrap()
            .is_empty());

        assert_eq!(backends(&db), vec![("a".into(), "wsl:MagicDebian".into())]);
        assert_eq!(
            bookmarks(&db),
            vec![("wsl:MagicDebian".into(), "/repo".into(), "keep".into())]
        );
    }

    /// `(host, repo_path)` is the bookmark key, so a path bookmarked both
    /// before the bug and during it collides on the rewrite. The pair is one
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
            .apply_wsl_repair_plan(&to_local(&["wsl:MagicDebian"]))
            .unwrap();
        assert_eq!(report.bookmarks_local, 2);
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
    /// spellings bookmarking one repo both rewrite to `host = ''`, which is
    /// one BINARY primary key for two rows. Raising SQLITE_CONSTRAINT here
    /// would repeat on every start.
    #[test]
    fn two_loopback_spellings_of_one_repo_do_not_violate_the_key() {
        let db = db();
        bookmark(&db, "wsl:Ubuntu", "/repo", "older", 100);
        bookmark(&db, "wsl:ubuntu", "/repo", "newer", 200);
        bookmark(&db, "wsl:UBUNTU", "/other", "only", 100);

        let report = db
            .apply_wsl_repair_plan(&to_local(&["wsl:Ubuntu", "wsl:ubuntu"]))
            .unwrap();
        assert_eq!(report.bookmarks_local, 2);
        assert_eq!(report.bookmarks_superseded, 1);

        assert_eq!(
            bookmarks(&db),
            vec![
                ("".into(), "/other".into(), "only".into()),
                ("".into(), "/repo".into(), "newer".into()),
            ]
        );
    }

    /// A rename carries the rows onto another host's backend name, and its
    /// collisions resolve the same way — with the row already at the
    /// destination playing the part the local row plays for a heal.
    #[test]
    fn a_rename_moves_rows_onto_the_destination_backend() {
        let db = db();
        session(&db, "a", "wsl:MagicDebian");
        session(&db, "b", "local-tmux");
        bookmark(&db, "wsl:MagicDebian", "/only-source", "keep", 100);
        bookmark(&db, "wsl:MagicDebian", "/both", "keep", 200);
        bookmark(&db, "wsl:MagicDebianPerso", "/both", "drop", 100);
        bookmark(&db, "", "/both", "keep", 300);

        let report = db
            .apply_wsl_repair_plan(&WslRepairPlan {
                to_local: Vec::new(),
                renames: vec![(
                    "wsl:MagicDebian".to_string(),
                    "wsl:MagicDebianPerso".to_string(),
                )],
            })
            .unwrap();

        assert_eq!((report.sessions_moved, report.sessions_local), (1, 0));
        assert_eq!(report.bookmarks_moved, 2);
        assert_eq!(report.bookmarks_superseded, 1);
        assert_eq!(
            backends(&db),
            vec![
                ("a".into(), "wsl:MagicDebianPerso".into()),
                ("b".into(), "local-tmux".into()),
            ]
        );
        assert_eq!(
            bookmarks(&db),
            vec![
                // The local row for the same path is a different key: a
                // rename's destination is not local, so it is left alone.
                ("".into(), "/both".into(), "keep".into()),
                ("wsl:MagicDebianPerso".into(), "/both".into(), "keep".into()),
                (
                    "wsl:MagicDebianPerso".into(),
                    "/only-source".into(),
                    "keep".into()
                ),
            ]
        );
    }

    /// `to_local` runs first, and that is what keeps the two arms from
    /// disagreeing: a loopback named after a rename's destination sends its
    /// own rows local without carrying the renamed ones along with them.
    #[test]
    fn a_heal_named_after_a_renames_destination_does_not_swallow_it() {
        let db = db();
        session(&db, "a", "wsl:MagicDebianPerso");
        session(&db, "b", "wsl:MagicDebian");

        db.apply_wsl_repair_plan(&WslRepairPlan {
            to_local: vec!["wsl:MagicDebianPerso".to_string()],
            renames: vec![(
                "wsl:MagicDebian".to_string(),
                "wsl:MagicDebianPerso".to_string(),
            )],
        })
        .unwrap();

        assert_eq!(
            backends(&db),
            vec![
                ("a".into(), "local-tmux".into()),
                ("b".into(), "wsl:MagicDebianPerso".into()),
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
