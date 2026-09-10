//! The one-time repair of rows a WSL loopback host recorded as remote.
//!
//! Two halves live apart on purpose. Schema v47 only *marks* the repair as
//! owed, because what to rewrite is decided by the host registry — which
//! backend names a loopback entry can have written, and which of those a host
//! thurbox still serves claims — and `storage` may not read `hosts.toml`. The
//! plan is built by
//! `agent::host_config::wsl_repair_plan` and handed to
//! [`Database::apply_wsl_repair_plan`] by
//! `session_ops::repair_wsl_loopback_rows`. What stays here is the SQL, which
//! is this module's job whoever decides the policy.

use rusqlite::{params, OptionalExtension};

use crate::session::{WslRepairPlan, LOCAL_BACKEND_TYPE};

use super::Database;

/// Metadata key set by schema v47 to record that the repair is owed, and
/// cleared once it has run.
pub(super) const WSL_LOOPBACK_REPAIR_OWED_KEY: &str = "wsl_loopback_repair_owed";

/// The local bookmark `host`: `repo_bookmarks` spells "this machine" as the
/// empty string, where `sessions.backend_type` spells it
/// [`LOCAL_BACKEND_TYPE`].
const LOCAL_BOOKMARK_HOST: &str = "";

/// What one repair pass changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WslLoopbackRepair {
    /// Sessions restored as this machine's own.
    pub sessions_local: usize,
    /// Bookmarks restored as this machine's own.
    pub bookmarks_local: usize,
    /// Bookmarks deleted because another reading of the same path won on
    /// recency — `(host, repo_path)` is the key, so only one can survive.
    pub bookmarks_superseded: usize,
}

/// A `repo_bookmarks` row in one heal's collision group, as the resolution
/// below reads it.
struct Candidate<'a> {
    host: &'a str,
    last_used_at: i64,
    /// Whether this row is already this machine's own, so it needs no rewrite
    /// and wins a tie.
    local: bool,
    is_parent: bool,
    parent_path: Option<&'a str>,
}

impl Candidate<'_> {
    /// Descending sort key: most recent first, a tie kept by the row that is
    /// already local, and a tie between two spellings of the loopback broken
    /// by name so the outcome does not depend on row order.
    fn precedence(&self) -> (i64, bool, std::cmp::Reverse<&str>) {
        (self.last_used_at, self.local, std::cmp::Reverse(self.host))
    }
}

/// Restore every row recorded under `from` (matched case-insensitively, the
/// way `wsl.exe -d` matches a distro name, so several spellings of one distro
/// heal together) as this machine's own.
///
/// The bookmark half cannot be a bare `UPDATE`: `(host, repo_path)` is the
/// primary key and it is BINARY, so `wsl:Ubuntu`, `wsl:ubuntu` and a local row
/// can all hold the same `repo_path`, and the rewrite would collide. Every
/// such group is one local path read several ways, so it is resolved on
/// recency — most recent reading wins, a tie keeps the row that is already
/// local — and the losers are deleted before the survivor is rewritten. A
/// `use_count` merge was considered and rejected: this is an MRU hint.
///
/// The survivor also inherits the group's place in the bookmark tree
/// (`is_parent` if any reading carried it, the most recent `parent_path`),
/// because the resolution is per `repo_path` while a parent's children are
/// rows of their own: without the merge a surviving parent could lose the mark
/// its children hang off, or a surviving child its filing under a parent.
///
/// Returns `(sessions, bookmarks, bookmarks_superseded)`.
fn heal_rows(
    tx: &rusqlite::Transaction<'_>,
    from: &str,
) -> rusqlite::Result<(usize, usize, usize)> {
    let sessions = tx.execute(
        "UPDATE sessions SET backend_type = ?1 WHERE backend_type = ?2 COLLATE NOCASE",
        params![LOCAL_BACKEND_TYPE, from],
    )?;

    let rows: Vec<(String, String, i64, bool, Option<String>)> = tx
        .prepare(
            "SELECT host, repo_path, last_used_at, is_parent, parent_path FROM repo_bookmarks",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut by_path: std::collections::BTreeMap<&str, Vec<Candidate>> =
        std::collections::BTreeMap::new();
    for (host, repo_path, last_used_at, is_parent, parent_path) in &rows {
        let local = host == LOCAL_BOOKMARK_HOST;
        if local || host.eq_ignore_ascii_case(from) {
            by_path.entry(repo_path).or_default().push(Candidate {
                host,
                last_used_at: *last_used_at,
                local,
                is_parent: *is_parent,
                parent_path: parent_path.as_deref(),
            });
        }
    }

    let mut healed = 0;
    let mut superseded = 0;
    for (repo_path, mut group) in by_path {
        if !group.iter().any(|c| !c.local) {
            continue;
        }
        group.sort_by(|a, b| b.precedence().cmp(&a.precedence()));
        let is_parent = group.iter().any(|c| c.is_parent);
        let parent_path = group.iter().find_map(|c| c.parent_path);
        let (winner, losers) = group.split_first().expect("group is never empty");
        for loser in losers {
            tx.execute(
                "DELETE FROM repo_bookmarks WHERE host = ?1 AND repo_path = ?2",
                params![loser.host, repo_path],
            )?;
            superseded += 1;
        }
        tx.execute(
            "UPDATE repo_bookmarks SET host = ?1, is_parent = ?2, parent_path = ?3 \
             WHERE host = ?4 AND repo_path = ?5",
            params![
                LOCAL_BOOKMARK_HOST,
                is_parent as i64,
                parent_path,
                winner.host,
                repo_path
            ],
        )?;
        if !winner.local {
            healed += 1;
        }
    }
    Ok((sessions, healed, superseded))
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

    /// How many sessions and repo bookmarks are recorded under any of
    /// `backends`, matched case-insensitively the way the heal matches them.
    ///
    /// What the repair *withheld* is only worth telling the user about when
    /// something is actually recorded there: an entry that claims a spelling
    /// but never wrote a row is an ordinary config, and the notice is the one
    /// message its owner would ever see.
    pub fn rows_recorded_on(&self, backends: &[String]) -> rusqlite::Result<(usize, usize)> {
        if backends.is_empty() {
            return Ok((0, 0));
        }
        let list = std::iter::repeat("?")
            .take(backends.len())
            .collect::<Vec<_>>()
            .join(", ");
        let count = |sql: String| -> rusqlite::Result<usize> {
            let n: i64 =
                self.conn
                    .query_row(&sql, rusqlite::params_from_iter(backends.iter()), |row| {
                        row.get(0)
                    })?;
            Ok(n as usize)
        };
        Ok((
            count(format!(
                "SELECT COUNT(*) FROM sessions WHERE backend_type COLLATE NOCASE IN ({list})"
            ))?,
            count(format!(
                "SELECT COUNT(*) FROM repo_bookmarks WHERE host COLLATE NOCASE IN ({list})"
            ))?,
        ))
    }

    /// Apply `plan`: rows recorded under a
    /// [`to_local`](WslRepairPlan::to_local) name become this machine's own
    /// (`backend_type` = [`LOCAL_BACKEND_TYPE`], bookmark `host` = `''`). One
    /// transaction, so a database is never left half-repaired.
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
            let (sessions, bookmarks, superseded) = heal_rows(&tx, from)?;
            report.sessions_local += sessions;
            report.bookmarks_local += bookmarks;
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
            withheld: Vec::new(),
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

    fn parent(db: &Database, host: &str) {
        db.conn
            .execute(
                "UPDATE repo_bookmarks SET is_parent = 1 WHERE host = ?1 AND repo_path = '/parent'",
                params![host],
            )
            .unwrap();
    }

    fn child(db: &Database, host: &str, repo_path: &str, parent_path: &str) {
        db.conn
            .execute(
                "UPDATE repo_bookmarks SET parent_path = ?3 WHERE host = ?1 AND repo_path = ?2",
                params![host, repo_path, parent_path],
            )
            .unwrap();
    }

    /// Every *local* bookmark as `(repo_path, is_parent, parent_path)`: a row
    /// left remote is absent, so asserting the whole list also asserts the
    /// heal reached all of them.
    fn tree(db: &Database) -> Vec<(String, bool, Option<String>)> {
        db.conn
            .prepare(
                "SELECT repo_path, is_parent, parent_path FROM repo_bookmarks \
                 WHERE host = '' ORDER BY repo_path",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
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

        assert_eq!(
            db.apply_wsl_repair_plan(&WslRepairPlan::default()).unwrap(),
            WslLoopbackRepair::default()
        );

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

    /// A bookmark can be a persisted *parent* with child rows filed under it
    /// (`parent_path`). The collision resolution is per `repo_path`, so
    /// without merging the group's tree columns the survivor could keep the
    /// recency it won on and lose the tree it belongs to — a parent no longer
    /// marked as one, or a child no longer filed under its parent. Whichever
    /// reading of a path wins, the subtree survives with it.
    #[test]
    fn a_healed_bookmark_keeps_its_place_in_the_tree() {
        let db = db();
        // The parent: the loopback row is the more recent reading, so it wins
        // and must carry the local row's `is_parent`.
        bookmark(&db, "", "/parent", "local", 100);
        parent(&db, "");
        bookmark(&db, "wsl:MagicDebian", "/parent", "loopback", 200);
        // A child: the local reading wins on recency and must inherit the
        // loopback row's filing under the parent.
        bookmark(&db, "", "/parent/child", "local", 300);
        bookmark(&db, "wsl:MagicDebian", "/parent/child", "loopback", 200);
        child(&db, "wsl:MagicDebian", "/parent/child", "/parent");
        // A child with no local rival moves as it is.
        bookmark(&db, "wsl:MagicDebian", "/parent/other", "loopback", 100);
        child(&db, "wsl:MagicDebian", "/parent/other", "/parent");

        db.apply_wsl_repair_plan(&to_local(&["wsl:MagicDebian"]))
            .unwrap();

        assert_eq!(
            tree(&db),
            vec![
                ("/parent".to_string(), true, None),
                (
                    "/parent/child".to_string(),
                    false,
                    Some("/parent".to_string())
                ),
                (
                    "/parent/other".to_string(),
                    false,
                    Some("/parent".to_string())
                ),
            ],
            "one local parent with both its children still filed under it"
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
