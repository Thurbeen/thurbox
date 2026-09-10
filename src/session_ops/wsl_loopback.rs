//! The one-time repair of rows a WSL loopback host recorded as remote.
//!
//! Schema v47 only marks the repair as owed. It has to: what to rewrite is
//! decided by the *registry* — which names a loopback entry can have written,
//! and which of those a host thurbox still serves claims — and `storage` may
//! not read `hosts.toml` or run `wsl.exe`. This is the layer
//! that sees both, so this is where the two halves meet: the plan from
//! [`crate::agent::host_config::wsl_repair_plan`], the SQL from
//! [`crate::storage::Database::apply_wsl_repair_plan`].
//!
//! Driven from **every** startup that opens the database — the TUI boot and
//! the `thurbox-cli` entrypoint — because the mark is written by any binary
//! that opens it, and a headless-driven install need never launch the
//! interface. Until it runs, a mislabelled row reads as remote: a reap sweep
//! refuses to kill windows it believes are on another machine, and leaks them.

use crate::storage::Database;

/// Perform the repair if it is owed, returning startup notices.
///
/// Runs at most once per database, and the mark is what guarantees it: read
/// first so an invocation with nothing to do pays a single query, and cleared
/// once a pass has an **answer**.
///
/// A withheld spelling is an answer, and the distinction matters because the
/// opposite reading looks reasonable. Rows under a name a live host claims are
/// permanently indistinguishable — that host's own remote rows and the local
/// rows an older release mislabelled look identical — so no later start can
/// classify them better than this one. Keeping the mark until the claim
/// disappears would not settle them; it would rewrite them once the evidence
/// was merely *gone*, relabelling a live sibling's sessions local, which is
/// the misassignment the withholding exists to prevent. So the pass applies
/// what it can and retires.
///
/// Only a question that could not be *asked* keeps the mark: an unreadable
/// `hosts.toml`, distros that could not be enumerated, a failed write. Those
/// are transient, and the answer is still out there. Best-effort throughout: a
/// database that cannot be repaired must still open.
pub fn repair_wsl_loopback_rows(db: &Database) -> Vec<String> {
    match db.wsl_loopback_repair_owed() {
        Ok(false) => return Vec::new(),
        Ok(true) => {}
        Err(e) => {
            tracing::warn!("could not read the WSL repair mark: {e}");
            return Vec::new();
        }
    }

    // A registry that could not be read is not "no hosts configured": what
    // keeps a live host's rows out of the heal is that entry being *seen*, and
    // losing it would relabel a sibling's live sessions as local.
    let plan = match crate::agent::host_config::wsl_repair_plan() {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!(
                "WSL row repair deferred — the host registry could not be settled \
                 ({e}); it stays owed and runs once that is answerable"
            );
            return Vec::new();
        }
    };

    let report = match db.apply_wsl_repair_plan(&plan) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!("WSL row repair failed, will retry on next start: {e}");
            return Vec::new();
        }
    };
    if let Err(e) = db.clear_wsl_loopback_repair_owed() {
        tracing::warn!("WSL row repair ran but its mark could not be cleared: {e}");
    }

    let mut notices = Vec::new();
    if !plan.withheld.is_empty() {
        notices.push(format!(
            "sessions recorded on {} were left as they are, for good: a host thurbox \
             serves registers under that name, so a local session an older release \
             mislabelled there cannot be told from one of that host's own, and \
             guessing either way would operate on the wrong machine",
            plan.withheld.join(", ")
        ));
    }
    if report.sessions_local > 0 || report.bookmarks_local > 0 {
        notices.push(format!(
            "{} session(s) and {} repo bookmark(s) were recorded on the WSL distro \
             thurbox runs in, which is this machine; restored them as local",
            report.sessions_local, report.bookmarks_local
        ));
    }
    if report.bookmarks_superseded > 0 {
        notices.push(format!(
            "{} repo bookmark(s) dropped as a staler reading of a path another row \
             already records",
            report.bookmarks_superseded
        ));
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::host_config::with_discovered_wsl;
    use crate::session::host_def::with_wsl_distro;
    use crate::session::HostDef;

    /// A discovery stub that fails if it is consulted at all: the repair must
    /// reach its answer without `wsl.exe` whenever no candidate is spelled
    /// after another distro, and a stubbed failure is a transient one, so any
    /// test using this would heal nothing if the narrowing regressed.
    fn discovery_is_not_consulted() -> Result<Vec<HostDef>, String> {
        Err("wsl.exe must not be consulted here".to_string())
    }

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
            with_discovered_wsl(discovery_is_not_consulted(), || {
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
        });
    }

    /// A hand-written loopback registers under its own `name`, so its rows are
    /// spelled `wsl:self` and the base `wsl:<us>` would miss them — stranding
    /// them on a host `hosts.toml` no longer describes.
    #[test]
    fn a_differently_named_loopbacks_rows_are_healed_too() {
        with_wsl_distro(Some("MagicDebian"), || {
            // `wsl:self` is spelled after no distro this machine has, so the
            // list is what says nothing claims it.
            with_discovered_wsl(Ok(vec![HostDef::wsl("MagicDebianPerso")]), || {
                let rig =
                    rig("[[hosts]]\nname = \"self\"\nkind = \"wsl\"\ndistro = \"MagicDebian\"\n");
                session(&rig.db, "a", "wsl:self");
                session(&rig.db, "b", "wsl:MagicDebian");
                session(&rig.db, "c", "wsl:MagicDebianPerso");

                repair_wsl_loopback_rows(&rig.db);

                assert_eq!(backend(&rig.db, "a"), "local-tmux");
                assert_eq!(backend(&rig.db, "b"), "local-tmux");
                assert_eq!(backend(&rig.db, "c"), "wsl:MagicDebianPerso");
            });
        });
    }

    /// A shadow entry records its sessions under the same name the loopback
    /// bug wrote, and nothing tells the two apart — so the repair leaves every
    /// row under that name exactly as it found it, in both directions: no
    /// sibling session is relabelled local, and no local one is moved onto the
    /// sibling. That is the answer for those rows, so the repair retires.
    #[test]
    fn a_shadow_configs_rows_are_left_exactly_as_they_are() {
        with_wsl_distro(Some("MagicDebian"), || {
            with_discovered_wsl(discovery_is_not_consulted(), || {
                let rig = rig("[[hosts]]\nname = \"MagicDebian\"\nkind = \"wsl\"\n\
                     distro = \"MagicDebianPerso\"\n");
                session(&rig.db, "a", "wsl:MagicDebian");
                rig.db
                    .conn_ref()
                    .execute(
                        "INSERT INTO repo_bookmarks (host, repo_path, last_used_at) \
                         VALUES ('wsl:MagicDebian', '/repo', 1)",
                        [],
                    )
                    .unwrap();

                let notices = repair_wsl_loopback_rows(&rig.db);

                assert_eq!(backend(&rig.db, "a"), "wsl:MagicDebian");
                assert_eq!(bookmark_hosts(&rig.db), ["wsl:MagicDebian"]);
                assert!(
                    notices
                        .iter()
                        .any(|n| n.contains("wsl:MagicDebian") && n.contains("for good")),
                    "the notice names the spelling and says it is final: {notices:?}"
                );
                assert!(
                    !rig.db.wsl_loopback_repair_owed().unwrap(),
                    "a withheld spelling is an answer, so the repair does not stay owed"
                );
            });
        });
    }

    /// The regression the withheld-mark policy caused: a shadow's rows are
    /// *genuinely remote*, and the claim disappearing does not make them
    /// readable — it only removes the evidence. A repair still owed at that
    /// point would relabel live sibling sessions local, so it must already be
    /// retired.
    #[test]
    fn removing_the_claiming_entry_does_not_relabel_its_rows_later() {
        with_wsl_distro(Some("MagicDebian"), || {
            with_discovered_wsl(discovery_is_not_consulted(), || {
                let rig = rig("[[hosts]]\nname = \"MagicDebian\"\nkind = \"wsl\"\n\
                     distro = \"MagicDebianPerso\"\n");
                session(&rig.db, "a", "wsl:MagicDebian");

                repair_wsl_loopback_rows(&rig.db);
                assert_eq!(backend(&rig.db, "a"), "wsl:MagicDebian");

                // The user acts on the warning and drops the entry.
                let path = crate::agent::host_config::hosts_config_path().unwrap();
                std::fs::write(&path, "").unwrap();

                assert!(repair_wsl_loopback_rows(&rig.db).is_empty());

                assert_eq!(
                    backend(&rig.db, "a"),
                    "wsl:MagicDebian",
                    "its windows are in the sibling distro; local-tmux would be the \
                     wrong machine"
                );
            });
        });
    }

    /// Distros that could not be enumerated are a question that could not be
    /// asked, not an answer: nothing is touched, the mark survives, and the
    /// work completes once `wsl.exe` answers.
    #[test]
    fn an_unenumerable_wsl_keeps_the_mark_and_completes_later() {
        with_wsl_distro(Some("MagicDebian"), || {
            let toml = "[[hosts]]\nname = \"self\"\nkind = \"wsl\"\ndistro = \"MagicDebian\"\n";
            let rig = with_discovered_wsl(Err("wsl.exe -l -q failed".to_string()), || {
                let rig = rig(toml);
                session(&rig.db, "a", "wsl:self");

                assert!(repair_wsl_loopback_rows(&rig.db).is_empty());

                assert_eq!(backend(&rig.db, "a"), "wsl:self");
                assert!(
                    rig.db.wsl_loopback_repair_owed().unwrap(),
                    "the mark survives, so the repair is not lost"
                );
                rig
            });

            with_discovered_wsl(Ok(Vec::new()), || {
                repair_wsl_loopback_rows(&rig.db);

                assert_eq!(backend(&rig.db, "a"), "local-tmux");
                assert!(!rig.db.wsl_loopback_repair_owed().unwrap());
            });
        });
    }

    /// The shadow withholds its own spelling and nothing else: a loopback
    /// alongside it is still healed under its own distinct name.
    #[test]
    fn a_loopback_is_still_healed_alongside_a_shadow() {
        with_wsl_distro(Some("MagicDebian"), || {
            with_discovered_wsl(Ok(vec![HostDef::wsl("MagicDebianPerso")]), || {
                let rig = rig("[[hosts]]\nname = \"MagicDebian\"\nkind = \"wsl\"\n\
                     distro = \"MagicDebianPerso\"\n\n\
                     [[hosts]]\nname = \"self\"\nkind = \"wsl\"\ndistro = \"MagicDebian\"\n");
                session(&rig.db, "a", "wsl:MagicDebian");
                session(&rig.db, "b", "wsl:self");

                repair_wsl_loopback_rows(&rig.db);

                assert_eq!(backend(&rig.db, "a"), "wsl:MagicDebian");
                assert_eq!(backend(&rig.db, "b"), "local-tmux");
            });
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
    /// rewrite to `host = ''`, which is one BINARY primary key for two rows.
    /// A SQLITE_CONSTRAINT here would repeat on every start.
    #[test]
    fn two_case_variant_loopbacks_bookmarking_one_repo_do_not_fail() {
        with_wsl_distro(Some("Ubuntu"), || {
            with_discovered_wsl(discovery_is_not_consulted(), || {
                let rig =
                    rig("[[hosts]]\nname = \"ubuntu\"\nkind = \"wsl\"\ndistro = \"Ubuntu\"\n");
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
        });
    }

    /// A `hosts.toml` that does not parse says nothing about who owns
    /// `wsl:<us>`, and acting on that silence would relabel a shadow host's
    /// live sibling sessions as local. So the pass changes nothing and stays
    /// owed, and the next start — once the file parses — does the work.
    #[test]
    fn an_unparseable_hosts_toml_defers_the_repair_and_changes_nothing() {
        with_wsl_distro(Some("MagicDebian"), || {
            with_discovered_wsl(discovery_is_not_consulted(), || {
                let rig = rig("[[hosts]\nname = \"MagicDebian\"\n");
                session(&rig.db, "a", "wsl:MagicDebian");

                assert!(repair_wsl_loopback_rows(&rig.db).is_empty());

                assert_eq!(backend(&rig.db, "a"), "wsl:MagicDebian");
                assert!(
                    rig.db.wsl_loopback_repair_owed().unwrap(),
                    "the mark survives, so the repair is not lost"
                );

                // Once the file parses, the same call does the work.
                let path = crate::agent::host_config::hosts_config_path().unwrap();
                std::fs::write(&path, "").unwrap();
                repair_wsl_loopback_rows(&rig.db);
                assert_eq!(backend(&rig.db, "a"), "local-tmux");
                assert!(!rig.db.wsl_loopback_repair_owed().unwrap());
            });
        });
    }
}
