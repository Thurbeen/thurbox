//! The one-time repair of rows a WSL loopback host recorded as remote.
//!
//! Schema v47 only marks the repair as owed. It has to: the rows to put back
//! are the ones written under a backend name the *registry* refuses, and
//! `storage` may not read `hosts.toml`. This is the layer that sees both, so
//! this is where the two halves meet — the set from
//! [`crate::agent::host_config::wsl_loopback_backend_names`], the SQL from
//! [`crate::storage::Database::relabel_wsl_loopback_rows`].

use crate::storage::Database;

/// Perform the loopback repair if it is owed, returning startup notices.
///
/// Runs at most once per database: the mark is cleared on success and left in
/// place on failure, so a transient error is retried on the next start rather
/// than losing the repair. Best-effort throughout — a database that cannot be
/// repaired must still open.
pub fn repair_wsl_loopback_rows(db: &Database) -> Vec<String> {
    match db.wsl_loopback_repair_owed() {
        Ok(false) => return Vec::new(),
        Ok(true) => {}
        Err(e) => {
            tracing::warn!("could not read the WSL loopback repair mark: {e}");
            return Vec::new();
        }
    }

    let heal = crate::agent::host_config::wsl_loopback_backend_names();
    let report = match db.relabel_wsl_loopback_rows(&heal) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!("WSL loopback repair failed, will retry on next start: {e}");
            return Vec::new();
        }
    };
    if let Err(e) = db.clear_wsl_loopback_repair_owed() {
        tracing::warn!("WSL loopback repair ran but its mark could not be cleared: {e}");
    }

    if report.is_empty() {
        return Vec::new();
    }
    let mut notices = Vec::new();
    if report.sessions > 0 {
        notices.push(format!(
            "{} session(s) were recorded on the WSL distro thurbox runs in, which is \
             this machine; restored them as local",
            report.sessions
        ));
    }
    if report.bookmarks > 0 || report.bookmarks_superseded > 0 {
        notices.push(format!(
            "{} repo bookmark(s) restored as local ({} superseded by a more recent \
             reading of the same path)",
            report.bookmarks, report.bookmarks_superseded
        ));
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::host_def::with_wsl_distro;

    struct Rig {
        _temp: tempfile::TempDir,
        _guard: crate::paths::TestPathGuard,
        db: Database,
    }

    /// A database owing the repair, with `hosts_toml` as the profile's
    /// `hosts.toml`.
    fn rig(hosts_toml: &str) -> Rig {
        let temp = tempfile::TempDir::new().unwrap();
        let guard = crate::paths::TestPathGuard::new(temp.path());
        let path = crate::agent::host_config::hosts_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, hosts_toml).unwrap();

        let db = Database::open_in_memory().unwrap();
        db.conn_ref()
            .execute(
                "INSERT INTO metadata (key, value) VALUES ('wsl_loopback_repair_owed', '1') \
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
                [],
            )
            .unwrap();
        Rig {
            _temp: temp,
            _guard: guard,
            db,
        }
    }

    fn session(db: &Database, id: &str, backend_type: &str) {
        db.conn_ref()
            .execute(
                "INSERT INTO sessions (id, name, agent, backend_type, backend_id, \
                 created_at, updated_at) VALUES (?1, ?1, 'claude', ?2, '%1', 0, 0)",
                rusqlite::params![id, backend_type],
            )
            .unwrap();
    }

    fn backend(db: &Database, id: &str) -> String {
        db.conn_ref()
            .query_row(
                "SELECT backend_type FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn bookmark_hosts(db: &Database) -> Vec<String> {
        db.conn_ref()
            .prepare("SELECT host FROM repo_bookmarks ORDER BY host, repo_path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn the_discovered_loopbacks_rows_are_healed_and_a_siblings_are_not() {
        with_wsl_distro(Some("MagicDebian"), || {
            let rig = rig("");
            session(&rig.db, "a", "wsl:MagicDebian");
            session(&rig.db, "b", "wsl:MagicDebianPerso");
            session(&rig.db, "c", "ssh:devbox");

            let notices = repair_wsl_loopback_rows(&rig.db);

            assert_eq!(backend(&rig.db, "a"), "local-tmux");
            assert_eq!(backend(&rig.db, "b"), "wsl:MagicDebianPerso");
            assert_eq!(backend(&rig.db, "c"), "ssh:devbox");
            assert!(
                notices.iter().any(|n| n.contains("restored them as local")),
                "the repair reports what it moved: {notices:?}"
            );
            assert!(!rig.db.wsl_loopback_repair_owed().unwrap());
            // Owed once: a second pass finds nothing to do and says nothing.
            assert!(repair_wsl_loopback_rows(&rig.db).is_empty());
        });
    }

    /// The union arm. A hand-written loopback registers under its own `name`,
    /// so its rows are spelled `wsl:self` and the base `wsl:<us>` would miss
    /// them — stranding them on a host `hosts.toml` no longer describes.
    #[test]
    fn a_differently_named_loopbacks_rows_are_healed_too() {
        with_wsl_distro(Some("MagicDebian"), || {
            let rig = rig("[[hosts]]\nname = \"self\"\nkind = \"wsl\"\ndistro = \"MagicDebian\"\n");
            session(&rig.db, "a", "wsl:self");
            session(&rig.db, "b", "wsl:MagicDebian");
            session(&rig.db, "c", "wsl:MagicDebianPerso");

            repair_wsl_loopback_rows(&rig.db);

            assert_eq!(backend(&rig.db, "a"), "local-tmux");
            assert_eq!(backend(&rig.db, "b"), "local-tmux");
            assert_eq!(backend(&rig.db, "c"), "wsl:MagicDebianPerso");
        });
    }

    /// The subtraction arm. While a shadow entry exists, `wsl:<us>` reaches a
    /// *sibling*: those sessions really are remote, and relabelling them local
    /// would run every attach, diff and delete on the wrong machine.
    #[test]
    fn a_shadow_configs_remote_rows_are_left_alone() {
        with_wsl_distro(Some("MagicDebian"), || {
            let rig = rig("[[hosts]]\nname = \"MagicDebian\"\nkind = \"wsl\"\n\
                 distro = \"MagicDebianPerso\"\n");
            session(&rig.db, "a", "wsl:MagicDebian");

            let notices = repair_wsl_loopback_rows(&rig.db);

            assert_eq!(backend(&rig.db, "a"), "wsl:MagicDebian");
            assert!(notices.is_empty(), "nothing was owed: {notices:?}");
            // The mark still clears, so the no-op is not retried forever.
            assert!(!rig.db.wsl_loopback_repair_owed().unwrap());
        });
    }

    #[test]
    fn off_wsl_the_repair_touches_nothing() {
        with_wsl_distro(None, || {
            let rig = rig("");
            session(&rig.db, "a", "wsl:MagicDebian");
            rig.db
                .conn_ref()
                .execute(
                    "INSERT INTO repo_bookmarks (host, repo_path, last_used_at) \
                     VALUES ('wsl:MagicDebian', '/repo', 1)",
                    [],
                )
                .unwrap();

            assert!(repair_wsl_loopback_rows(&rig.db).is_empty());

            // A Windows (or plain Linux) thurbox driving that distro is the
            // case the repair must not touch.
            assert_eq!(backend(&rig.db, "a"), "wsl:MagicDebian");
            assert_eq!(bookmark_hosts(&rig.db), ["wsl:MagicDebian"]);
        });
    }

    /// Two case-variant spellings of the loopback bookmarking one repo both
    /// relabel to `host = ''`, which is one BINARY primary key for two rows.
    /// A SQLITE_CONSTRAINT here would repeat on every start.
    #[test]
    fn two_case_variant_loopbacks_bookmarking_one_repo_do_not_fail() {
        with_wsl_distro(Some("Ubuntu"), || {
            let rig = rig("[[hosts]]\nname = \"ubuntu\"\nkind = \"wsl\"\ndistro = \"Ubuntu\"\n");
            rig.db
                .conn_ref()
                .execute_batch(
                    "INSERT INTO repo_bookmarks (host, repo_path, label, last_used_at) VALUES
                        ('wsl:Ubuntu', '/repo', 'older', 100),
                        ('wsl:ubuntu', '/repo', 'newer', 200);",
                )
                .unwrap();

            repair_wsl_loopback_rows(&rig.db);

            assert_eq!(bookmark_hosts(&rig.db), [""], "one local row survives");
            let label: String = rig
                .db
                .conn_ref()
                .query_row("SELECT label FROM repo_bookmarks", [], |r| r.get(0))
                .unwrap();
            assert_eq!(label, "newer", "resolved on recency");
            assert!(!rig.db.wsl_loopback_repair_owed().unwrap());
        });
    }
}
