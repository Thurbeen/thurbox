//! Shared sessions through the public surface: the host-facing CLI verbs a
//! peer drives (`list --deleted`, `register`, `sync`), the one JSON shape both
//! directions use, the mirror's reconciliation rules over a real database, and
//! the `if_missing` relaunch the loop asks for. The ssh half is a scripted
//! runner in the unit tests beside `session_ops::host_cli`; here nothing is
//! faked but `ssh` itself, which one test replaces on `PATH` with a stand-in
//! running the real `thurbox-cli` against a second data directory.

use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use serde_json::{json, Value};
use thurbox::cli::sessions::{run, Action};
use thurbox::kernel::command::{Args, Command};
use thurbox::session::{HostDef, SessionId};
use thurbox::session_ops::mirror::{self, HostDeletedRow, MirrorReport, Transitive};
use thurbox::storage::Database;
use thurbox::sync::{SharedSession, SharedWorktree};

/// The guard every tmux server in this file is reaped by — see its own doc.
#[cfg(unix)]
#[path = "support/tmux_server.rs"]
mod tmux_server;

const BACKEND: &str = "ssh:devbox";

/// `register` shells out to list windows on this machine's tmux server, so
/// the refusal it gives only exercises "no live window" where a `tmux`
/// binary exists to ask; on a runner without one (Windows CI) the call fails
/// before it gets that far, the same gate other tmux-backed tests use.
fn have_tmux() -> bool {
    ProcessCommand::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn row(id: SessionId, name: &str, backend: &str) -> SharedSession {
    SharedSession {
        id,
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%3".into(),
        backend_type: backend.into(),
        agent_session_id: Some("conv".into()),
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    }
}

#[test]
fn list_deleted_prints_what_a_mirroring_peer_reads() {
    let db = Database::open_in_memory().unwrap();
    let kept = SessionId::default();
    db.upsert_session(&row(kept, "kept", "local-tmux")).unwrap();
    let gone = SessionId::default();
    db.upsert_session(&row(gone, "gone", "local-tmux")).unwrap();
    db.soft_delete_session(gone).unwrap();
    db.mark_session_force_deleted(gone).unwrap();

    let out = run(
        Action::List {
            include_purged: false,
            parent: None,
            recursive: false,
            tree: false,
            deleted: true,
            since: None,
            verify: false,
            key: None,
            key_prefix: None,
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap();
    let rows = out.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], gone.to_string());
    assert_eq!(rows[0]["force_deleted"], true);
    assert_eq!(rows[0]["backend_type"], "local-tmux");
    assert!(out.human.contains("in part"), "{}", out.human);

    // The active list carries the fields the mirror needs and nothing it
    // would have to guess: the pane, the extra dirs, the base branch.
    let active = run(
        Action::List {
            include_purged: false,
            parent: None,
            recursive: false,
            tree: false,
            deleted: false,
            since: None,
            verify: false,
            key: None,
            key_prefix: None,
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap();
    let only = &active.as_array().unwrap()[0];
    assert_eq!(only["id"], kept.to_string());
    assert_eq!(only["backend_id"], "%3");
    assert!(only["additional_dirs"].is_array());
    assert!(only.get("base_branch").is_some());
    assert!(only.get("hook_state").is_some());
}

#[test]
fn the_json_a_host_prints_is_the_json_a_peer_reads() {
    let id = SessionId::default();
    let mut s = row(id, "shape", "local-tmux");
    s.additional_dirs = vec![PathBuf::from("/srv/extra")];
    let printed = mirror::session_to_json(&s, Some("done"), Some("main"), Some(1));
    let read = mirror::session_from_json(&printed, BACKEND).unwrap();
    assert_eq!(read.session.id, id);
    assert_eq!(
        read.session.backend_type, BACKEND,
        "the observer's name for the host"
    );
    assert_eq!(read.session.additional_dirs, s.additional_dirs);
    assert_eq!(read.hook_state.as_deref(), Some("done"));
    assert_eq!(read.base_branch.as_deref(), Some("main"));
}

/// The key is part of the wire format (issue #1408): a peer that mirrors a
/// host takes each row's key with it, and a row without one stays without.
#[test]
fn a_key_survives_the_round_trip_through_the_wire_format() {
    let mut s = row(SessionId::default(), "lead", "local-tmux");
    s.key = Some("fleet.acme.mission-control".into());
    let printed = mirror::session_to_json(&s, None, None, Some(1));
    assert_eq!(printed["key"], "fleet.acme.mission-control");
    let read = mirror::session_from_json(&printed, BACKEND).unwrap();
    assert_eq!(read.session.key, s.key);

    // A host older than keys prints no field, and has none.
    let mut legacy = printed.clone();
    legacy.as_object_mut().unwrap().remove("key");
    assert_eq!(
        mirror::session_from_json(&legacy, BACKEND)
            .unwrap()
            .session
            .key,
        None
    );
}

/// The host's record of a key wins on every pass — given, changed and taken
/// away there — and a local session with the same key is no conflict: keys are
/// unique per host, and the two are on two machines.
#[test]
fn a_mirror_pass_carries_the_hosts_key_and_its_changes() {
    let host = Database::open_in_memory().unwrap();
    let observer = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    let mut theirs = row(id, "lead", "local-tmux");
    theirs.key = Some("fleet.lead".into());
    host.upsert_session(&theirs).unwrap();
    let mut mine = row(SessionId::default(), "lead", "local-tmux");
    mine.key = Some("fleet.lead".into());
    observer.upsert_session(&mine).unwrap();

    let key_here = |db: &Database| db.get_session_by_id(id).unwrap().unwrap().key;
    let report = pass(&observer, BACKEND, &host);
    assert_eq!(report.adopted, vec![id]);
    assert_eq!(key_here(&observer).as_deref(), Some("fleet.lead"));

    host.set_session_key(id, Some("fleet.lead.v2")).unwrap();
    assert_eq!(pass(&observer, BACKEND, &host).updated, vec![id]);
    assert_eq!(key_here(&observer).as_deref(), Some("fleet.lead.v2"));

    host.set_session_key(id, None).unwrap();
    pass(&observer, BACKEND, &host);
    assert_eq!(key_here(&observer), None);
    assert_eq!(
        observer.get_session_by_id(mine.id).unwrap().unwrap().key,
        mine.key,
        "the observer's own session is not the host's to re-key"
    );
}

#[test]
fn register_records_only_a_window_that_is_running() {
    // No tmux server is reachable from a test, so the one answer `register`
    // can give is the refusal — which is the property: it records, it never
    // launches, and it will not invent a row for a window that is not there.
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    let body = mirror::session_to_json(&row(id, "elsewhere", "local-tmux"), None, None, None);
    let err = run(
        Action::Register {
            json_row: body.to_string(),
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap_err();
    assert!(err.contains("no live window"), "{err}");
    assert!(db.get_session_by_id(id).unwrap().is_none());

    let err = run(
        Action::Register {
            json_row: "{not json".into(),
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap_err();
    assert!(err.contains("--json-row"), "{err}");
}

#[test]
fn register_refuses_an_id_or_a_name_already_here() {
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    db.upsert_session(&row(id, "taken", "local-tmux")).unwrap();
    let same_id = mirror::session_to_json(&row(id, "other", "local-tmux"), None, None, None);
    let err = run(
        Action::Register {
            json_row: same_id.to_string(),
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap_err();
    assert!(err.contains("already registered"), "{err}");
    let same_name = mirror::session_to_json(
        &row(SessionId::default(), "taken", "local-tmux"),
        None,
        None,
        None,
    );
    let err = run(
        Action::Register {
            json_row: same_name.to_string(),
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap_err();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn sync_with_no_shareable_host_configured_is_an_empty_report() {
    let temp = tempfile::TempDir::new().unwrap();
    let _guard = thurbox::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    let out = run(
        Action::Sync {
            host: None,
            adopt: false,
        },
        &db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap();
    assert_eq!(out.as_array().map(Vec::len), Some(0));
    assert!(out.human.contains("No shareable hosts"), "{}", out.human);
}

#[test]
fn the_mirror_reconciles_a_real_database_both_ways() {
    let db = Database::open_in_memory().unwrap();
    let theirs = SessionId::default();
    // A low `updated_at` for the adopt: the restore leg below needs a *later*
    // reading of the host's own clock to outrank the local tombstone (schema
    // v45 orders a restore against the host's last-known value, not against
    // this machine's `deleted_at` — see `mirror::apply`).
    let host_rows = vec![mirror::session_from_json(
        &mirror::session_to_json(
            &row(theirs, "theirs", "local-tmux"),
            Some("working"),
            None,
            Some(1),
        ),
        BACKEND,
    )
    .unwrap()];
    let report = mirror::apply(&db, BACKEND, &host_rows, &[]);
    assert_eq!(report.adopted, vec![theirs]);
    let adopted = db.get_session_by_id(theirs).unwrap().unwrap();
    assert_eq!(adopted.backend_type, BACKEND);

    // The host deletes it; the peer's row follows, recoverable in part.
    let report = mirror::apply(
        &db,
        BACKEND,
        &[],
        &[HostDeletedRow {
            lifecycle_generation: None,
            purged: false,
            id: theirs,
            force_deleted: true,
        }],
    );
    assert_eq!(report.deleted, vec![theirs]);
    assert!(
        db.get_deleted_session_by_id(theirs)
            .unwrap()
            .unwrap()
            .force_deleted
    );

    // The host restores it; so does the peer. A restore only outranks the
    // tombstone when the host's own clock reads past its last-known value, so
    // this leg needs a fresh, later `updated_at`.
    let host_rows = vec![mirror::session_from_json(
        &mirror::session_to_json(
            &row(theirs, "theirs", "local-tmux"),
            Some("working"),
            None,
            Some(2),
        ),
        BACKEND,
    )
    .unwrap()];
    let report = mirror::apply(&db, BACKEND, &host_rows, &[]);
    assert_eq!(report.restored, vec![theirs]);
    assert!(db.get_session_by_id(theirs).unwrap().is_some());

    // A row on another backend is never the mirror's business.
    let local = SessionId::default();
    db.upsert_session(&row(local, "mine", "local-tmux"))
        .unwrap();
    let report = mirror::apply(&db, BACKEND, &host_rows, &[]);
    assert!(!report.changed());
    assert!(report.unknown_local.is_empty());
}

#[test]
fn a_host_entry_shares_by_default_and_can_opt_out() {
    let on: HostDef = toml::from_str("name = \"devbox\"\ndestination = \"me@devbox\"").unwrap();
    assert!(on.shareable());
    let off: HostDef =
        toml::from_str("name = \"devbox\"\ndestination = \"me@devbox\"\nshare_sessions = false")
            .unwrap();
    assert!(!off.shareable());
}

#[test]
fn a_restart_asked_by_a_plugin_is_a_full_restart_never_a_relaunch() {
    // `if_missing` is the loop's word, for the agent-is-gone case; a plugin's
    // "restart" must keep meaning kill-and-relaunch.
    let parsed = Command::parse(
        "restart",
        Args {
            session: "s".into(),
            ..Args::default()
        },
    )
    .unwrap();
    assert!(matches!(
        parsed,
        Command::Restart {
            if_missing: false,
            ..
        }
    ));
    assert_eq!(parsed.session(), "s");
    let _ = json!({});
}

// Mirroring across more than one hop: A mirrors B, B mirrors C, and A may also
// mirror C directly. Each instance is a real database, and every listing
// crosses as the JSON `session list [--deleted] --json` prints - the bytes
// `mirror::mirror_host` reads over ssh - so only the ssh hop is not real. A
// session keeps its id across every hop, which is what the mirror dedupes on.

const LOCAL: &str = "local-tmux";
/// Each instance's name for the others. Made up, and deliberately not the
/// same spelling everywhere: B's name for C need not be A's.
const A_TO_B: &str = "ssh:bravo";
const A_TO_C: &str = "ssh:charlie";
const B_TO_C: &str = "ssh:c-from-bravo";
const B_TO_A: &str = "ssh:alpha";

fn own(db: &Database, name: &str) -> SessionId {
    let id = SessionId::default();
    db.upsert_session(&SharedSession {
        id,
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%1".into(),
        backend_type: LOCAL.into(),
        agent_session_id: None,
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: vec![SharedWorktree {
            repo_path: PathBuf::from("/srv/repo"),
            worktree_path: PathBuf::from(format!("/srv/worktrees/{name}")),
            branch: name.into(),
            created_by_thurbox: true,
        }],
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    })
    .unwrap();
    id
}

/// What `session list --json` and `session list --deleted --json` print.
fn listing(db: &Database, deleted: bool) -> Value {
    run(
        Action::List {
            parent: None,
            recursive: false,
            tree: false,
            deleted,
            include_purged: false,
            since: None,
            verify: false,
            key: None,
            key_prefix: None,
        },
        db,
        &thurbox::cli::Backends::ready(thurbox::backend::wiring::configured().0),
    )
    .unwrap()
    .json
}

/// One mirror pass of `host` into `observer`, on the observer's name for it.
fn pass(observer: &Database, backend: &str, host: &Database) -> MirrorReport {
    mirror::reconcile_with(
        observer,
        backend,
        &listing(host, false),
        &listing(host, true),
        Transitive::Show,
    )
}

/// [`pass`] with `[remote] transitive_sessions = false`.
fn pass_hiding(observer: &Database, backend: &str, host: &Database) -> MirrorReport {
    mirror::reconcile_with(
        observer,
        backend,
        &listing(host, false),
        &listing(host, true),
        Transitive::Hide,
    )
}

/// Where the observer's `session list --json` puts `id`: one backend per
/// appearance, so a session listed twice shows up as two entries.
fn listed_on(observer: &Database, id: SessionId) -> Vec<String> {
    listing(observer, false)
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == id.to_string())
        .map(|row| row["backend_type"].as_str().unwrap().to_string())
        .collect()
}

/// B holds its own session and a mirror of C's; C holds its own.
struct Chain {
    a: Database,
    b: Database,
    c: Database,
    on_b: SessionId,
    on_c: SessionId,
}

fn chain() -> Chain {
    let (a, b, c) = (
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
    );
    let on_b = own(&b, "bravo-own");
    let on_c = own(&c, "charlie-own");
    pass(&b, B_TO_C, &c);
    assert_eq!(listed_on(&b, on_c), vec![B_TO_C], "B mirrors C");
    Chain {
        a,
        b,
        c,
        on_b,
        on_c,
    }
}

#[test]
fn a_session_reached_directly_and_through_a_host_is_listed_once_on_its_owner() {
    let Chain {
        a,
        b,
        c,
        on_b,
        on_c,
        ..
    } = chain();

    // Whichever order the passes run in, and however many of them, C's
    // session is C's: A reaches it directly, so that is the path it keeps.
    pass(&a, A_TO_B, &b);
    pass(&a, A_TO_C, &c);
    for _ in 0..2 {
        let through_b = pass(&a, A_TO_B, &b);
        assert!(
            !through_b.adopted.contains(&on_c) && !through_b.updated.contains(&on_c),
            "the pass through B moved C's session: {through_b:?}"
        );
        pass(&a, A_TO_C, &c);
        assert_eq!(listed_on(&a, on_c), vec![A_TO_C]);
    }
    assert_eq!(listed_on(&a, on_b), vec![A_TO_B], "B's own is still B's");
}

#[test]
fn a_session_only_reachable_through_a_host_is_listed_through_it() {
    let Chain {
        a, b, on_b, on_c, ..
    } = chain();
    for _ in 0..2 {
        pass(&a, A_TO_B, &b);
        assert_eq!(listed_on(&a, on_b), vec![A_TO_B]);
        assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);

        // B's own row names B's pane and B's checkout, and both are B's.
        let bs = a.get_session_by_id(on_b).unwrap().unwrap();
        assert_eq!(bs.backend_id, "%1");
        assert!(bs.worktrees[0].created_by_thurbox);
        // C's does not: `%1` there is a pane on C's server, and on B's it is
        // B's own agent — attaching to it would type into the wrong session.
        // Its checkout is C's, so nothing run on B may remove it.
        let cs = a.get_session_by_id(on_c).unwrap().unwrap();
        assert_eq!(cs.backend_id, "", "the second pass keeps it clear too");
        assert!(!cs.worktrees[0].created_by_thurbox);
        assert_eq!(cs.worktrees[0].branch, "charlie-own", "still described");
    }
}

#[test]
fn a_host_that_mirrors_us_back_never_relabels_our_own_session() {
    let (a, b) = (
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
    );
    let mine = own(&a, "alpha-own");
    pass(&b, B_TO_A, &a);
    pass(&a, A_TO_B, &b);
    assert_eq!(listed_on(&a, mine), vec![LOCAL]);
}

#[test]
fn a_session_deleted_on_its_direct_path_is_not_revived_through_another() {
    let Chain { a, b, c, on_c, .. } = chain();
    pass(&a, A_TO_C, &c);
    // Deleted here; B has not mirrored C since, so it still lists it active.
    a.soft_delete_session(on_c).unwrap();
    let through_b = pass(&a, A_TO_B, &b);
    assert!(listed_on(&a, on_c).is_empty(), "{through_b:?}");
    assert!(
        through_b.tombstoned.is_empty(),
        "nothing to push to B: the delete is C's to hear, on C's path"
    );
}

#[test]
fn hiding_transitive_sessions_lists_only_each_hosts_own() {
    let Chain {
        a, b, on_b, on_c, ..
    } = chain();
    let report = pass_hiding(&a, A_TO_B, &b);
    assert_eq!(report.adopted, vec![on_b]);
    assert!(listed_on(&a, on_c).is_empty());
}

#[test]
fn hiding_forgets_what_was_taken_on_without_touching_it_and_showing_brings_it_back() {
    let Chain { a, b, c, on_c, .. } = chain();
    pass(&a, A_TO_B, &b);
    assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);

    let report = pass_hiding(&a, A_TO_B, &b);
    assert_eq!(report.forgotten, vec![on_c]);
    assert!(report.tombstoned.is_empty() && report.unknown_local.is_empty());
    assert!(listed_on(&a, on_c).is_empty());
    let deleted_here = listing(&a, true);
    assert_eq!(
        deleted_here.as_array().map(Vec::len),
        Some(0),
        "forgotten, not deleted: {deleted_here}"
    );
    assert!(
        pass_hiding(&a, A_TO_B, &b).tombstoned.is_empty(),
        "and nothing is pushed to B on the next pass"
    );
    assert_eq!(listed_on(&b, on_c), vec![B_TO_C], "B's mirror is B's");
    assert_eq!(listed_on(&c, on_c), vec![LOCAL], "the session lives on");

    let report = pass(&a, A_TO_B, &b);
    assert_eq!(report.adopted, vec![on_c]);
    assert_eq!(listed_on(&a, on_c), vec![A_TO_B]);
}

#[test]
fn hiding_leaves_a_session_reached_directly_where_it_is() {
    let Chain { a, b, c, on_c, .. } = chain();
    pass(&a, A_TO_C, &c);
    let report = pass_hiding(&a, A_TO_B, &b);
    assert!(report.forgotten.is_empty(), "{report:?}");
    assert_eq!(listed_on(&a, on_c), vec![A_TO_C]);
}

#[cfg(unix)]
fn host_row(id: SessionId, name: &str) -> SharedSession {
    SharedSession {
        id,
        name: name.into(),
        agent: "claude".into(),
        backend_id: String::new(),
        backend_type: "tmux".into(),
        agent_session_id: None,
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
        key: None,
    }
}

/// An `ssh` that drops its options and destination and runs the remote
/// command with `sh`, as the host's login shell would, inside the host's own
/// thurbox directories. `host-bin` comes first on the host's `PATH`, so a test
/// can stand a different `thurbox-cli` in front of the real one.
#[cfg(unix)]
fn write_ssh(bin: &std::path::Path, root: &std::path::Path) {
    write_ssh_with(bin, root, "");
}

/// [`write_ssh`], with `extra` shell lines run on the "host" just before the
/// remote command — where a test points it at its own multiplexer, or makes
/// the link misbehave.
#[cfg(unix)]
fn write_ssh_with(bin: &std::path::Path, root: &std::path::Path, extra: &str) {
    let cli_dir = std::path::Path::new(env!("CARGO_BIN_EXE_thurbox-cli"))
        .parent()
        .unwrap()
        .display()
        .to_string();
    let script = format!(
        r#"#!/bin/sh
while [ $# -gt 0 ]; do
    case "$1" in
        -o) shift 2 ;;
        -*) shift ;;
        *) shift; break ;;
    esac
done
printf '%s\n' "$*" >> '{root}/ssh.log'
export HOME='{root}/host-home'
export THURBOX_CONFIG_DIR='{root}/host-config'
export THURBOX_DATA_DIR='{root}/host-data'
export PATH='{root}/host-bin':'{cli_dir}':/usr/bin:/bin
{extra}
exec sh -c "$*"
"#,
        root = root.display(),
    );
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, script).unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// How many full listings have crossed the wire so far.
#[cfg(unix)]
fn listings(root: &std::path::Path) -> usize {
    std::fs::read_to_string(root.join("ssh.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.replace('\'', "").contains("session list"))
        .count()
}

/// The mirror over a real `ssh` round trip: an idle host is not re-listed.
///
/// A host's full `session list` + `session list --deleted` grows with its
/// deleted history, and the mirror runs every few seconds per host. The
/// stand-in logs every remote script, so the test sees what crossed the wire.
/// It sets `PATH` and the data directories process-wide, which nextest's
/// process per test keeps to this test.
#[cfg(unix)]
#[test]
fn an_idle_host_is_not_listed_again() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for dir in [
        "bin",
        "home",
        "config",
        "data",
        "host-home",
        "host-config",
        "host-data",
    ] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    write_ssh(&root.join("bin"), root);
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("THURBOX_CONFIG_DIR", root.join("config"));
    std::env::set_var("THURBOX_DATA_DIR", root.join("data"));
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    // The host: one live session and a long deleted history.
    let host_db = Database::open(&root.join("host-data/thurbox.db")).unwrap();
    let live = SessionId::default();
    host_db.upsert_session(&host_row(live, "live")).unwrap();
    for n in 0..50 {
        let gone = SessionId::default();
        host_db
            .upsert_session(&host_row(gone, &format!("gone-{n}")))
            .unwrap();
        host_db.soft_delete_session(gone).unwrap();
    }

    let host = HostDef {
        name: "probe".into(),
        destination: "probe".into(),
        ..Default::default()
    };
    let cli = match thurbox::session_ops::host_cli::usable(&host) {
        thurbox::session_ops::host_cli::Usable::Yes(cli) => cli,
        thurbox::session_ops::host_cli::Usable::No(why) => {
            panic!("the stand-in host is not usable: {why}")
        }
    };
    let db = Database::open(&root.join("data/thurbox.db")).unwrap();

    let first = mirror::mirror_host(&db, &host, &cli).unwrap();
    assert_eq!(first.adopted, vec![live], "{first:?}");
    let after_first = listings(root);
    assert!(after_first > 0, "the first pass lists the host");

    let idle = mirror::mirror_host(&db, &host, &cli).unwrap();
    assert!(!idle.changed(), "{idle:?}");
    assert_eq!(
        listings(root),
        after_first,
        "a pass over a host that changed nothing pulled its full lists again"
    );

    // A change on the host is still picked up on the very next pass.
    host_db.soft_delete_session(live).unwrap();
    let moved = mirror::mirror_host(&db, &host, &cli).unwrap();
    assert_eq!(moved.deleted, vec![live], "{moved:?}");
    assert!(listings(root) > after_first);
}

/// A host that upgraded first is still shared with (#1429). Its CLI is a minor
/// release ahead — same major, one schema ahead — and an exact schema match
/// rejected it, so the peer tried to provision its own older CLI over the
/// host's and, where that failed (it does on Windows), fell back to spawning
/// straight onto the host's multiplexer with the row in its own database only:
/// a session the host's own thurbox never saw.
#[cfg(unix)]
#[test]
fn a_host_a_schema_ahead_in_the_same_major_is_delegated_to() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for dir in [
        "bin",
        "home",
        "config",
        "data",
        "host-bin",
        "host-home",
        "host-config",
        "host-data",
    ] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    write_ssh(&root.join("bin"), root);
    // The host's own CLI, reporting the schema of the next minor release.
    let ours = thurbox::storage::SCHEMA_VERSION;
    let newer = root.join("host-bin/thurbox-cli");
    std::fs::write(
        &newer,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = version ]; then\n\
             '{real}' \"$@\" | sed 's/\"schema_version\":{ours}/\"schema_version\":{next}/'\n\
             exit\n\
             fi\n\
             exec '{real}' \"$@\"\n",
            real = env!("CARGO_BIN_EXE_thurbox-cli"),
            next = ours + 1,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&newer, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("THURBOX_CONFIG_DIR", root.join("config"));
    std::env::set_var("THURBOX_DATA_DIR", root.join("data"));
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    let host = HostDef {
        name: "probe".into(),
        destination: "probe".into(),
        ..Default::default()
    };
    match thurbox::session_ops::host_cli::usable(&host) {
        thurbox::session_ops::host_cli::Usable::Yes(cli) => {
            assert_eq!(cli.schema_version, Some(ours + 1), "{cli:?}");
            // The host's own CLI, not a copy of this peer's provisioned
            // over it to open the host's newer database.
            assert_eq!(cli.path, newer.display().to_string(), "{cli:?}");
        }
        thurbox::session_ops::host_cli::Usable::No(why) => {
            panic!("a host one minor release ahead is not shared with: {why}")
        }
    }
}

/// A session created on a host that shares its sessions is created by the
/// host's own `thurbox-cli` — and must come out the way the caller asked for
/// it (#1471).
///
/// Two real instances: the caller, and the "host" behind [`write_ssh_with`],
/// each with its own config, data, home and tmux server. Only the network hop
/// is not real: every flag the caller forwards is parsed by the real host CLI,
/// and stdin crosses the stand-in as it would cross ssh.
#[cfg(unix)]
mod delegated_create {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use thurbox::session::SessionId;
    use thurbox::storage::Database;

    use super::tmux_server::TmuxServer;

    const SETTINGS: &str =
        "[features]\nautomations = false\nversion_check = false\nauto_update = false\n";

    /// The host's registry: a `shell` default that idles, and an agent that
    /// pins a conversation id through `resume_args`.
    const HOST_AGENTS: &str = "default = \"shell\"\n\n\
         [[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = [\"-c\", \"exec sleep 600\"]\n\n\
         [[agents]]\nname = \"resumer\"\ncommand = \"sh\"\nargs = []\n\
         resume_args = [\"-c\", \"true {id}; exec sleep 600\"]\nresume_latest = false\n";

    /// The caller, wired to reach the host through the stand-in `ssh`.
    struct Pair {
        root: tempfile::TempDir,
        caller_server: TmuxServer,
        host_server: TmuxServer,
    }

    impl Pair {
        fn new() -> Self {
            let root = tempfile::TempDir::new().expect("tempdir");
            for sub in [
                "bin",
                "home",
                "config",
                "data",
                "work",
                "host-home",
                "host-config",
                "host-data",
            ] {
                std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
            }
            let path = |sub: &str| root.path().join(sub);
            std::fs::write(path("config/settings.toml"), SETTINGS).unwrap();
            std::fs::write(path("host-config/settings.toml"), SETTINGS).unwrap();
            std::fs::write(path("host-config/agents.toml"), HOST_AGENTS).unwrap();
            std::fs::write(
                path("config/hosts.toml"),
                "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
            )
            .unwrap();
            let caller_server = TmuxServer::private("tbx-dc-caller");
            let host_server = TmuxServer::private("tbx-dc-host");
            // The host's own multiplexer, and a link that can be made to drop
            // the answer of a `session create` the host has already carried out.
            super::write_ssh_with(
                &path("bin"),
                root.path(),
                &format!(
                    "export TMUX_TMPDIR='{tmpdir}' THURBOX_SOCKET='{socket}'\n\
                     unset THURBOX_SOCKET_FOR THURBOX_SESSION THURBOX_SESSION_ID\n\
                     case \"$*\" in\n\
                     \x20 *' session revision '*)\n\
                     \x20   if [ -e '{race_arm}' ]; then\n\
                     \x20     printf started > '{race_started}'; race_wait=0\n\
                     \x20     while [ ! -e '{race_go}' ] && [ $race_wait -lt 400 ]; do sleep 0.05; race_wait=$((race_wait + 1)); done\n\
                     \x20   fi ;;\n\
                     \x20 *' session fork '*)\n\
                     \x20   if [ -e '{fork_other}' ]; then cat '{fork_other}'; exit 0; fi ;;\n\
                     \x20 *' session create '*)\n\
                     \x20   if [ -e '{refuse}' ]; then exit 255; fi\n\
                     \x20   if [ -e '{drop}' ]; then sh -c \"$*\" >/dev/null 2>&1; exit 255; fi ;;\n\
                     \x20 *' session get '*)\n\
                     \x20   if [ -e '{other}' ]; then\n\
                     \x20     printf '%s\\n' '{{\"id\":\"00000000-0000-0000-0000-000000000001\"}}'; exit 0\n\
                     \x20   fi ;;\n\
                     esac",
                    race_arm = path("race-arm").display(),
                    race_started = path("race-started").display(),
                    race_go = path("race-go").display(),
                    fork_other = path("fork-answers-another").display(),
                    other = path("get-answers-another").display(),
                    tmpdir = host_server.tmpdir().display(),
                    socket = host_server.socket(),
                    drop = path("drop-answer").display(),
                    refuse = path("never-arrives").display(),
                ),
            );
            Self {
                root,
                caller_server,
                host_server,
            }
        }

        fn path(&self, sub: &str) -> PathBuf {
            self.root.path().join(sub)
        }

        fn host_db(&self) -> Database {
            Database::open(&self.path("host-data/thurbox.db")).expect("open the host database")
        }

        fn caller_db(&self) -> Database {
            Database::open(&self.path("data/thurbox.db")).expect("open the caller database")
        }

        /// `session create --host probe` from the caller.
        fn create(&self, args: &[&str]) -> Output {
            self.create_via("probe", args)
        }

        /// [`create`](Self::create), naming the host as `host`.
        fn create_via(&self, host: &str, args: &[&str]) -> Output {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            cmd.args(["session", "create", "--host", host, "--repo-path"])
                .arg(self.path("work"))
                .args(args)
                .arg("--json")
                .env("HOME", self.path("home"))
                .env("USERPROFILE", self.path("home"))
                .env("THURBOX_CONFIG_DIR", self.path("config"))
                .env("THURBOX_DATA_DIR", self.path("data"))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        self.path("bin").display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
                .env_remove("THURBOX_SESSION")
                .env_remove("THURBOX_SESSION_ID");
            self.caller_server.scope(&mut cmd);
            cmd.output().expect("run thurbox-cli")
        }

        /// `session create` run by the host's own CLI, as a thurbox there
        /// would.
        fn create_on_host(&self, args: &[&str]) -> Output {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            cmd.args(["session", "create", "--repo-path"])
                .arg(self.path("work"))
                .args(args)
                .arg("--json")
                .env("HOME", self.path("host-home"))
                .env("USERPROFILE", self.path("host-home"))
                .env("THURBOX_CONFIG_DIR", self.path("host-config"))
                .env("THURBOX_DATA_DIR", self.path("host-data"))
                .env_remove("THURBOX_SESSION")
                .env_remove("THURBOX_SESSION_ID");
            self.host_server.scope(&mut cmd);
            cmd.output().expect("run the host's thurbox-cli")
        }

        /// Every remote command the stand-in was handed.
        fn ssh_log(&self) -> String {
            std::fs::read_to_string(self.path("ssh.log")).unwrap_or_default()
        }
    }

    fn answer(out: &Output) -> Value {
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is JSON ({e}): {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn created_id(out: &Output) -> SessionId {
        let doc = answer(out);
        assert!(out.status.success(), "create failed: {doc}");
        doc["id"].as_str().expect("an id").parse().expect("a uuid")
    }

    /// Wait for `path` to hold something, for as long as a pane takes to
    /// start.
    fn wait_for_file(path: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            assert!(
                Instant::now() < deadline,
                "{} was never written",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn telemetry_attributes_are_discoverable_and_invalid_keys_do_not_create_sessions() {
        let help = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(["session", "create", "--help"])
            .output()
            .unwrap();
        assert!(help.status.success());
        assert!(String::from_utf8_lossy(&help.stdout).contains("--telemetry-attr"));
        let pair = Pair::new();
        for value in [
            "missing-equals",
            "=empty",
            " space=value",
            "line\nbreak=value",
        ] {
            let out = pair.create_on_host(&["--name", "invalid", "--telemetry-attr", value]);
            assert!(!out.status.success());
            assert!(answer(&out)["error"]
                .as_str()
                .unwrap()
                .contains("--telemetry-attr"));
            assert!(pair.host_db().list_active_sessions().unwrap().is_empty());
        }
    }

    #[test]
    fn delegated_telemetry_attributes_are_available_from_the_callers_session_context() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        for config in ["config/settings.toml", "host-config/settings.toml"] {
            std::fs::write(
                pair.path(config),
                format!("{SETTINGS}\n[telemetry]\nenabled = true\n"),
            )
            .unwrap();
        }
        std::fs::write(pair.path("config/agents.toml"), HOST_AGENTS).unwrap();
        let run = |args: &[&str]| {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            cmd.args(args)
                .env("HOME", pair.path("home"))
                .env("THURBOX_CONFIG_DIR", pair.path("config"))
                .env("THURBOX_DATA_DIR", pair.path("data"))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        pair.path("bin").display(),
                        std::env::var("PATH").unwrap()
                    ),
                );
            pair.caller_server.scope(&mut cmd);
            cmd.output().unwrap()
        };
        let id = created_id(&pair.create(&[
            "--name",
            "context",
            "--agent",
            "shell",
            "--agent-arg=-i",
            "--telemetry-attr",
            "factory.task.id=topic/task",
        ]));
        let plan = run(&[
            "agent",
            "launch-args",
            "shell",
            "--session",
            &id.to_string(),
            "--json",
        ]);
        assert!(
            plan.status.success(),
            "{}",
            String::from_utf8_lossy(&plan.stdout)
        );
        let attrs = answer(&plan)["env"]["OTEL_RESOURCE_ATTRIBUTES"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(attrs.contains("factory.task.id=topic%2Ftask"), "{attrs}");
        let fork = created_id(&run(&[
            "session",
            "fork",
            &id.to_string(),
            "--name",
            "context-fork",
            "--json",
        ]));
        let fork_meta = run(&[
            "session",
            "meta",
            "get",
            &fork.to_string(),
            "thurbox.telemetry_attributes",
            "--json",
        ]);
        assert!(fork_meta.status.success(), "{}", answer(&fork_meta));
        let caller_attrs: std::collections::BTreeMap<String, String> =
            serde_json::from_str(answer(&fork_meta)["value"].as_str().unwrap()).unwrap();
        assert_eq!(
            caller_attrs.get("factory.task.id").map(String::as_str),
            Some("topic/task")
        );
        let synced = run(&["session", "sync", "--host", "probe", "--adopt", "--json"]);
        assert!(
            synced.status.success(),
            "{}",
            String::from_utf8_lossy(&synced.stdout)
        );
        assert!(
            answer(&synced)[0]["registered"]
                .as_array()
                .unwrap()
                .is_empty(),
            "the fork is already owned by the host"
        );
        assert_eq!(
            pair.host_db().load_telemetry_attrs(fork).unwrap(),
            caller_attrs
        );
        assert_eq!(pair.host_db().load_agent_args(fork).unwrap(), ["-i"]);
        std::fs::write(pair.path("bin/ssh"), "#!/bin/sh\nexit 255\n").unwrap();
        for settings in [
            "[telemetry]\nenabled = false\n",
            "[telemetry]\nenabled = true\nconfigure_agents = false\n",
        ] {
            std::fs::write(
                pair.path("config/settings.toml"),
                format!("{SETTINGS}{settings}"),
            )
            .unwrap();
            let plan = run(&[
                "agent",
                "launch-args",
                "shell",
                "--session",
                &id.to_string(),
                "--json",
            ]);
            assert!(
                plan.status.success(),
                "telemetry is disabled but the launch plan failed: {}",
                String::from_utf8_lossy(&plan.stdout)
            );
            assert!(answer(&plan)["env"]
                .get("OTEL_RESOURCE_ATTRIBUTES")
                .is_none());
        }
    }

    #[test]
    fn telemetry_attributes_follow_intermediate_mirror_hosts() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        let owner = pair.path("owner");
        for dir in ["host-home", "host-config", "host-data", "host-bin"] {
            std::fs::create_dir_all(owner.join(dir)).unwrap();
        }
        std::fs::create_dir_all(pair.path("host-bin")).unwrap();
        let settings = format!(
            "{SETTINGS}\n[telemetry]\nenabled = true\n[remote]\ntransitive_sessions = true\n"
        );
        for config in [
            pair.path("config"),
            pair.path("host-config"),
            owner.join("host-config"),
        ] {
            std::fs::write(config.join("settings.toml"), &settings).unwrap();
            std::fs::write(config.join("agents.toml"), HOST_AGENTS).unwrap();
        }
        std::fs::write(
            pair.path("host-config/hosts.toml"),
            "[[hosts]]\nname = 'owner'\ndestination = 'owner'\nshare_sessions = true\n",
        )
        .unwrap();
        let owner_server = TmuxServer::private("tbx-attrs-owner");
        super::write_ssh_with(&pair.path("host-bin"), &owner, &format!(
            "export TMUX_TMPDIR='{}' THURBOX_SOCKET='{}'\nunset THURBOX_SOCKET_FOR THURBOX_SESSION THURBOX_SESSION_ID",
            owner_server.tmpdir().display(), owner_server.socket()));
        let run = |place: &str, args: &[&str]| {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            let (home, config, data, bin) = match place {
                "owner" => (
                    owner.join("host-home"),
                    owner.join("host-config"),
                    owner.join("host-data"),
                    owner.join("host-bin"),
                ),
                "host" => (
                    pair.path("host-home"),
                    pair.path("host-config"),
                    pair.path("host-data"),
                    pair.path("host-bin"),
                ),
                _ => (
                    pair.path("home"),
                    pair.path("config"),
                    pair.path("data"),
                    pair.path("bin"),
                ),
            };
            cmd.args(args)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("THURBOX_CONFIG_DIR", config)
                .env("THURBOX_DATA_DIR", data)
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .env_remove("THURBOX_SESSION")
                .env_remove("THURBOX_SESSION_ID");
            match place {
                "owner" => owner_server.scope(&mut cmd),
                "host" => pair.host_server.scope(&mut cmd),
                _ => pair.caller_server.scope(&mut cmd),
            };
            cmd.output().unwrap()
        };
        let created = run(
            "owner",
            &[
                "session",
                "create",
                "--name",
                "owned",
                "--repo-path",
                pair.path("work").to_str().unwrap(),
                "--agent",
                "shell",
                "--telemetry-attr",
                "factory.task.id=topic/owner",
                "--json",
            ],
        );
        let id = created_id(&created);
        for (place, host) in [("host", "owner"), ("caller", "probe")] {
            let out = run(place, &["session", "sync", "--host", host, "--json"]);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(answer(&out)[0]["error"].is_null(), "{}", answer(&out));
        }
        assert!(pair.caller_db().get_session_by_id(id).unwrap().is_some());
        assert!(pair
            .host_db()
            .get_session_meta(id, "thurbox.telemetry_attributes")
            .unwrap()
            .is_none());
        let plan = run(
            "caller",
            &[
                "agent",
                "launch-args",
                "shell",
                "--session",
                &id.to_string(),
                "--json",
            ],
        );
        assert!(
            plan.status.success(),
            "{}",
            String::from_utf8_lossy(&plan.stdout)
        );
        let attrs = answer(&plan)["env"]["OTEL_RESOURCE_ATTRIBUTES"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(attrs.contains("factory.task.id=topic%2Fowner"), "{attrs}");
        pair.caller_db()
            .set_telemetry_attrs(
                id,
                &[("factory.task.id".into(), "stale-observer".into())].into(),
            )
            .unwrap();
        let fresh = run(
            "caller",
            &[
                "agent",
                "launch-args",
                "shell",
                "--session",
                &id.to_string(),
                "--json",
            ],
        );
        assert!(fresh.status.success(), "{}", answer(&fresh));
        let fresh_attrs = answer(&fresh)["env"]["OTEL_RESOURCE_ATTRIBUTES"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            fresh_attrs.contains("factory.task.id=topic%2Fowner"),
            "{fresh_attrs}"
        );

        let fork = created_id(&run(
            "caller",
            &[
                "session",
                "fork",
                &id.to_string(),
                "--name",
                "relay-fork",
                "--json",
            ],
        ));
        let fork_meta = run(
            "caller",
            &[
                "session",
                "meta",
                "get",
                &fork.to_string(),
                "thurbox.telemetry_attributes",
                "--json",
            ],
        );
        assert!(fork_meta.status.success(), "{}", answer(&fork_meta));
        let fork_attrs: Value =
            serde_json::from_str(answer(&fork_meta)["value"].as_str().unwrap()).unwrap();
        assert_eq!(fork_attrs["factory.task.id"], "topic/owner");

        let key = "thurbox.telemetry_attributes";
        let fetched = run(
            "caller",
            &["session", "meta", "get", &id.to_string(), key, "--json"],
        );
        assert!(fetched.status.success());
        let labels: Value =
            serde_json::from_str(answer(&fetched)["value"].as_str().unwrap()).unwrap();
        assert_eq!(labels["factory.task.id"], "topic/owner");
        let exhausted = run(
            "host",
            &[
                "session",
                "meta",
                "get",
                &id.to_string(),
                key,
                "--telemetry-hops",
                "0",
                "--json",
            ],
        );
        assert!(!exhausted.status.success());
        assert!(answer(&exhausted)["error"]
            .as_str()
            .unwrap()
            .contains("hop limit"));
        let local = run(
            "owner",
            &[
                "session",
                "meta",
                "get",
                &id.to_string(),
                key,
                "--telemetry-hops",
                "0",
                "--json",
            ],
        );
        assert!(local.status.success(), "{}", answer(&local));
        let excessive = run(
            "owner",
            &[
                "session",
                "meta",
                "get",
                &id.to_string(),
                key,
                "--telemetry-hops",
                "17",
                "--json",
            ],
        );
        assert!(!excessive.status.success());
        assert!(answer(&excessive)["error"]
            .as_str()
            .unwrap()
            .contains("telemetry-hops"));
    }

    #[test]
    fn telemetry_launch_plan_uses_requested_agent_defaults() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        std::fs::write(
            pair.path("host-config/settings.toml"),
            format!("{SETTINGS}\n[telemetry]\nenabled = true\n"),
        )
        .unwrap();
        std::fs::write(
            pair.path("host-config/agents.toml"),
            r#"
default = "source"
[[agents]]
name = "source"
command = "sh"
args = ["-c", "exec sleep 600"]
[agents.telemetry_env]
OTEL_RESOURCE_ATTRIBUTES = "flavor=source,source.only=yes"
[[agents]]
name = "target"
command = "sh"
[agents.telemetry_env]
OTEL_RESOURCE_ATTRIBUTES = "flavor=target,target.only=yes"
"#,
        )
        .unwrap();
        let id = created_id(&pair.create_on_host(&[
            "--name",
            "selected",
            "--agent",
            "source",
            "--env",
            "OTEL_RESOURCE_ATTRIBUTES=session=kept",
            "--telemetry-attr",
            "factory.task.id=topic/task",
        ]));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args([
            "agent",
            "launch-args",
            "target",
            "--session",
            &id.to_string(),
            "--json",
        ])
        .env("HOME", pair.path("host-home"))
        .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
        .env("THURBOX_DATA_DIR", pair.path("host-data"));
        pair.host_server.scope(&mut cmd);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let attrs = answer(&out)["env"]["OTEL_RESOURCE_ATTRIBUTES"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(attrs.contains("flavor=target"), "{attrs}");
        assert!(attrs.contains("target.only=yes"), "{attrs}");
        assert!(!attrs.contains("source.only"), "{attrs}");
        assert!(attrs.contains("session=kept"), "{attrs}");
        assert!(attrs.contains("factory.task.id=topic%2Ftask"), "{attrs}");
    }

    #[test]
    fn a_failed_telemetry_attribute_write_rolls_back_session_creation() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        drop(pair.host_db());
        let connection = rusqlite::Connection::open(pair.path("host-data/thurbox.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER refuse_telemetry BEFORE INSERT ON session_meta
            WHEN NEW.key = 'thurbox.telemetry_attributes'
            BEGIN SELECT RAISE(ABORT, 'telemetry write refused'); END;",
            )
            .unwrap();
        let out = pair.create_on_host(&[
            "--name",
            "rollback",
            "--telemetry-attr",
            "factory.task.id=task",
        ]);
        assert!(!out.status.success());
        assert!(answer(&out)["error"]
            .as_str()
            .unwrap()
            .contains("telemetry write refused"));
        let db = pair.host_db();
        assert!(
            db.list_active_sessions().unwrap().is_empty(),
            "a failed telemetry write left a session behind"
        );
        assert!(
            db.session_events_for_export(0, 100).unwrap().is_empty(),
            "a failed telemetry write published an unlabelled creation"
        );
    }

    #[test]
    fn telemetry_attributes_reach_local_and_delegated_launches() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        std::fs::write(
            pair.path("host-config/settings.toml"),
            format!("{SETTINGS}\n[telemetry]\nenabled = true\n"),
        )
        .unwrap();
        std::env::set_var(
            "OTEL_RESOURCE_ATTRIBUTES",
            "deployment.environment.name=test,team=global",
        );
        for delegated in [false, true] {
            let seen = pair.path(if delegated {
                "delegated-attrs"
            } else {
                "local-attrs"
            });
            let script = format!(
                "printf '%s' \"$OTEL_RESOURCE_ATTRIBUTES\" > '{}'; exec sleep 600",
                seen.display()
            );
            let args = [
                "--name",
                if delegated { "delegated" } else { "local" },
                "--command",
                "sh",
                "--arg",
                "-c",
                "--arg",
                &script,
                "--env",
                "OTEL_RESOURCE_ATTRIBUTES=team=worker,factory.task.id=old",
                "--telemetry-attr",
                "factory.task.id=overwritten",
                "--telemetry-attr",
                "factory.task.id=topic/task,one=two",
                "--telemetry-attr",
                "thurbox.agent=spoofed",
                "--telemetry-attr=--tag=value",
            ];
            let out = if delegated {
                pair.create(&args)
            } else {
                pair.create_on_host(&args)
            };
            let id = created_id(&out);
            let attrs = wait_for_file(&seen);
            assert!(attrs.contains("team=worker"), "{attrs}");
            assert!(
                attrs.contains("deployment.environment.name=test"),
                "{attrs}"
            );
            assert!(!attrs.contains("team=global"), "{attrs}");
            assert!(
                attrs.contains("factory.task.id=topic%2Ftask%2Cone%3Dtwo"),
                "{attrs}"
            );
            assert!(
                attrs.contains(&format!("thurbox.session.id={id}")),
                "{attrs}"
            );
            assert!(!attrs.contains("spoofed"), "{attrs}");
            assert!(attrs.contains("--tag=value"), "{attrs}");
            let host = pair.host_db();
            let persisted = host
                .get_session_meta(id, "thurbox.telemetry_attributes")
                .unwrap()
                .unwrap();
            let saved: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&persisted).unwrap();
            assert_eq!(saved["factory.task.id"], "topic/task,one=two");
            let mut restart = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            restart
                .args(["session", "restart", &id.to_string(), "--json"])
                .env("HOME", pair.path("host-home"))
                .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
                .env("THURBOX_DATA_DIR", pair.path("host-data"));
            pair.host_server.scope(&mut restart);
            std::fs::remove_file(&seen).unwrap();
            let restarted = restart.output().unwrap();
            assert!(
                restarted.status.success(),
                "{}",
                String::from_utf8_lossy(&restarted.stdout)
            );
            assert_eq!(wait_for_file(&seen), attrs);
            let mut fork = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            fork.args([
                "session",
                "fork",
                &id.to_string(),
                "--name",
                if delegated {
                    "delegated-fork"
                } else {
                    "local-fork"
                },
                "--json",
            ])
            .env("HOME", pair.path("host-home"))
            .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
            .env("THURBOX_DATA_DIR", pair.path("host-data"));
            pair.host_server.scope(&mut fork);
            std::fs::remove_file(&seen).unwrap();
            let forked = fork.output().unwrap();
            let fork_id = created_id(&forked);
            let fork_attrs = wait_for_file(&seen);
            assert!(
                fork_attrs.contains("factory.task.id=topic%2Ftask%2Cone%3Dtwo"),
                "{fork_attrs}"
            );
            assert!(
                fork_attrs.contains(&format!("thurbox.session.id={fork_id}")),
                "{fork_attrs}"
            );
            assert!(!fork_attrs.contains(&id.to_string()), "{fork_attrs}");
        }
    }

    /// `--command`, `--arg`, `--env` and `--reports-as` reach the host's CLI;
    /// the environment's values never reach a command line on the way.

    #[test]
    fn a_caller_owned_remote_fork_keeps_its_environment_after_sharing_is_enabled() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        let hosts = pair.path("config/hosts.toml");
        let shared_hosts = std::fs::read_to_string(&hosts).unwrap();
        std::fs::write(&hosts, shared_hosts.replace("true", "false")).unwrap();
        let capture = pair.path("direct-account");
        let script = format!(
            "printf '%s' \"$ACCOUNT\" > '{}'; exec sleep 600",
            capture.display()
        );
        std::fs::write(pair.path("config/agents.toml"), format!(
            "default = 'account'\n[[agents]]\nname = 'account'\ncommand = 'sh'\nargs = ['-c', {script:?}]\nenv = {{ ACCOUNT = 'caller' }}\n"
        )).unwrap();
        let source = created_id(&pair.create(&["--name", "direct", "--agent", "account"]));
        assert_eq!(wait_for_file(&capture), "caller");
        assert!(pair.host_db().get_session_by_id(source).unwrap().is_none());
        std::fs::write(&hosts, shared_hosts).unwrap();
        std::fs::remove_file(&capture).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        command
            .args([
                "session",
                "fork",
                &source.to_string(),
                "--name",
                "direct-fork",
                "--json",
            ])
            .env("HOME", pair.path("home"))
            .env("THURBOX_CONFIG_DIR", pair.path("config"))
            .env("THURBOX_DATA_DIR", pair.path("data"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    pair.path("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID");
        pair.caller_server.scope(&mut command);
        let fork = created_id(&command.output().unwrap());
        assert_eq!(wait_for_file(&capture), "caller");
        let db = pair.caller_db();
        assert_eq!(db.load_launch_env(fork).unwrap()["ACCOUNT"], "caller");
        assert_eq!(
            db.get_session_by_id(fork)
                .unwrap()
                .unwrap()
                .parent_session_id,
            Some(source)
        );
        assert!(pair.host_db().get_session_by_id(fork).unwrap().is_none());
    }

    #[test]
    fn a_delegated_agent_uses_the_hosts_environment_and_session_overrides() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        let capture = pair.path("agent-env");
        let script = format!(
            "printf '%s\\n' \"$ACCOUNT\" \"$CHOICE\" \"$CONFIG_ROOT\" > '{}'; exec sleep 600",
            capture.display()
        );
        use std::os::unix::fs::PermissionsExt;
        let agent = pair.path("account-agent");
        std::fs::write(&agent, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hook_capture = pair.path("post-create.json");
        let hook_command = format!("cat > '{}'", hook_capture.display());
        std::fs::write(
            pair.path("config/hooks.toml"),
            format!("[[hooks]]\nevent = 'session.post_create'\ncommand = {hook_command:?}\n"),
        )
        .unwrap();
        std::fs::write(
            pair.path("host-config/agents.toml"),
            format!(
                r#"
default = "codex"
[[agents]]
name = "codex"
command = {agent_path:?}
resume_args = ["resume", "{{id}}"]
fork_args = ["fork", "{{id}}"]
env = {{ ACCOUNT = "host-account", CHOICE = "agent", CONFIG_ROOT = "{{home}}/account" }}
"#,
                agent_path = agent.display().to_string()
            ),
        )
        .unwrap();
        let host_def = std::fs::read_to_string(pair.path("host-config/agents.toml")).unwrap();
        std::fs::write(
            pair.path("config/agents.toml"),
            host_def
                .replace("host-account", "caller-account")
                .replace("fork_args = [\"fork\", \"{id}\"]", "fork_args = []"),
        )
        .unwrap();
        let out = pair.create(&[
            "--name",
            "account",
            "--agent",
            "codex",
            "--env",
            "CHOICE=session",
        ]);
        let id = created_id(&out);
        let env = pair.host_db().load_launch_env(id).unwrap();
        assert_eq!(env.get("ACCOUNT").map(String::as_str), Some("host-account"));
        assert_eq!(env["CHOICE"], "session");
        assert_eq!(
            env["CONFIG_ROOT"],
            pair.path("host-home/account").display().to_string()
        );
        let text = wait_for_file(&capture);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "host-account",
                "session",
                pair.path("host-home/account").to_str().unwrap()
            ]
        );
        assert!(
            !pair.ssh_log().contains("host-account"),
            "agent env must stay on the host"
        );
        pair.caller_db()
            .set_session_meta(id, "ui.note", "caller-note")
            .unwrap();
        pair.host_db()
            .set_session_meta(id, "ui.note", "host-note")
            .unwrap();
        let captured_parent = "00000000-0000-0000-0000-000000000123";
        pair.host_db()
            .set_session_meta(id, "thurbox.codex_conversation_id", captured_parent)
            .unwrap();
        std::fs::remove_file(&capture).unwrap();
        let mut fork = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        fork.args([
            "session",
            "fork",
            &id.to_string(),
            "--name",
            "account-fork",
            "--json",
        ])
        .env("HOME", pair.path("home"))
        .env("THURBOX_CONFIG_DIR", pair.path("config"))
        .env("THURBOX_DATA_DIR", pair.path("data"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                pair.path("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env_remove("THURBOX_SESSION")
        .env_remove("THURBOX_SESSION_ID");
        pair.caller_server.scope(&mut fork);
        let forked = fork.output().unwrap();
        let fork_id = created_id(&forked);
        assert_eq!(
            pair.host_db().load_launch_env(fork_id).unwrap()["ACCOUNT"],
            "host-account"
        );
        assert_eq!(wait_for_file(&capture).lines().next(), Some("host-account"));
        let mut problems = Vec::new();
        let caller_program = fork.get_program().to_os_string();
        let caller_env: Vec<_> = fork
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
            .collect();
        let caller = |args: &[&str]| {
            let mut command = Command::new(&caller_program);
            command.args(args);
            for (key, value) in &caller_env {
                match value {
                    Some(value) => {
                        command.env(key, value);
                    }
                    None => {
                        command.env_remove(key);
                    }
                }
            }
            command.output().unwrap()
        };
        let shown = caller(&["session", "show", &id.to_string(), "--no-verify", "--json"]);
        assert!(shown.status.success());
        if !answer(&shown)["env"].is_null() {
            problems.push("mirror reported an unknown saved environment as empty");
        }
        let plan = caller(&[
            "agent",
            "launch-args",
            "codex",
            "--session",
            &id.to_string(),
            "--json",
        ]);
        if !plan.status.success() || !answer(&plan)["env"]["ACCOUNT"].is_null() {
            problems.push("mirror launch plan failed or claimed an unknown account environment");
        }
        let exec = caller(&[
            "session",
            "exec",
            &id.to_string(),
            "--json",
            "--",
            "sh",
            "-c",
            "printf '%s' \"$ACCOUNT\"",
        ]);
        if exec.status.success()
            || !String::from_utf8_lossy(&exec.stdout).contains("saved environment")
        {
            problems.push("mirror exec used an unknown account environment");
        }
        let mut host_get = Command::new(fork.get_program());
        host_get
            .args(["session", "show", &id.to_string(), "--no-verify", "--json"])
            .env("HOME", pair.path("host-home"))
            .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
            .env("THURBOX_DATA_DIR", pair.path("host-data"))
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID");
        pair.host_server.scope(&mut host_get);
        let shown = host_get.output().unwrap();
        assert!(shown.status.success());
        assert_eq!(answer(&shown)["env"]["ACCOUNT"], "host-account");

        if pair
            .caller_db()
            .get_session_meta(fork_id, "ui.note")
            .unwrap()
            .as_deref()
            != Some("caller-note")
        {
            problems.push("caller fork lost its own published metadata");
        }
        assert_eq!(
            pair.host_db()
                .get_session_meta(fork_id, "ui.note")
                .unwrap()
                .as_deref(),
            Some("host-note")
        );

        if answer(&forked)["continues_conversation"] != true {
            problems.push("caller lost host conversation continuity");
        }
        let hook: Value =
            serde_json::from_str(&std::fs::read_to_string(&hook_capture).unwrap()).unwrap();
        if hook["fork_session_id"] != captured_parent {
            problems.push("caller post-create hook lost the host-selected fork conversation");
        }
        let mut local = pair.caller_db().get_session_by_id(id).unwrap().unwrap();
        local.id = "00000000-0000-0000-0000-000000000124".parse().unwrap();
        local.name = "local-holder".into();
        local.backend_type = "local:tmux".into();
        local.backend_id.clear();
        pair.caller_db().upsert_session(&local).unwrap();
        pair.caller_db()
            .set_session_meta(local.id, "ui.note", "sentinel")
            .unwrap();
        std::fs::write(
            pair.path("fork-answers-another"),
            format!(r#"{{"id":"{}","continues_conversation":false}}"#, local.id),
        )
        .unwrap();
        let forged = caller(&[
            "session",
            "fork",
            &id.to_string(),
            "--name",
            "forged",
            "--json",
        ]);
        if forged.status.success()
            || pair
                .caller_db()
                .get_session_meta(local.id, "ui.note")
                .unwrap()
                .as_deref()
                != Some("sentinel")
        {
            problems.push("host fork response took over a locally owned session");
        }
        std::fs::remove_file(pair.path("fork-answers-another")).unwrap();
        local.id = "00000000-0000-0000-0000-000000000125".parse().unwrap();
        local.name = "racing-holder".into();
        std::fs::write(
            pair.path("fork-answers-another"),
            format!(r#"{{"id":"{}","continues_conversation":false}}"#, local.id),
        )
        .unwrap();
        std::fs::write(pair.path("race-arm"), "armed").unwrap();
        let raced = std::thread::scope(|scope| {
            let request = scope.spawn(|| {
                caller(&[
                    "session",
                    "fork",
                    &id.to_string(),
                    "--name",
                    "racing",
                    "--json",
                ])
            });
            let deadline = Instant::now() + Duration::from_secs(20);
            while !pair.path("race-started").exists()
                && !request.is_finished()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            let reached = pair.path("race-started").exists();
            let inserted = if reached {
                let db = Database::open(&pair.path("data/thurbox.db")).unwrap();
                db.upsert_session(&local)
                    .and_then(|_| db.set_session_meta(local.id, "ui.note", "race-sentinel"))
            } else {
                Ok(())
            };
            std::fs::write(pair.path("race-go"), "go").unwrap();
            let answer = request.join().unwrap();
            assert!(reached, "fork never reached its post-response mirror read");
            inserted.unwrap();
            answer
        });
        if raced.status.success()
            || pair
                .caller_db()
                .get_session_meta(local.id, "ui.note")
                .unwrap()
                .as_deref()
                != Some("race-sentinel")
        {
            problems.push("fork mirror race took over a locally owned session");
        }
        std::fs::remove_file(pair.path("race-arm")).unwrap();
        std::fs::remove_file(pair.path("fork-answers-another")).unwrap();
        let pre_capture = pair.path("pre-create.json");
        let hook_command = format!("cat > '{}'", pre_capture.display());
        std::fs::write(
            pair.path("config/hooks.toml"),
            format!("[[hooks]]\nevent = 'session.pre_create'\ncommand = {hook_command:?}\n"),
        )
        .unwrap();
        assert!(!caller(&[
            "session",
            "fork",
            &id.to_string(),
            "--name",
            "../bad",
            "--json"
        ])
        .status
        .success());
        if pre_capture.exists() {
            problems.push("invalid fork name ran a pre-create hook");
        }
        std::fs::write(
            pair.path("config/hosts.toml"),
            "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = false\n",
        )
        .unwrap();
        let refused = fork.output().unwrap();
        if refused.status.success() {
            problems.push("mirror fork without owning CLI lost saved environment");
        }
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_delegated_create_carries_the_command_its_args_and_its_env() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let seen = pair.path("seen");
        let script = format!(
            "printf '%s' \"$FM_SECRET\" > '{}'; exec sleep 600",
            seen.display()
        );
        let out = pair.create(&[
            "--name",
            "carried",
            "--command",
            "sh",
            "--arg",
            "-c",
            "--arg",
            &script,
            "--env",
            "FM_SECRET=env-secret",
            "--reports-as",
            "claude",
        ]);
        let id = created_id(&out);

        assert_eq!(
            wait_for_file(&seen),
            "env-secret",
            "the host must launch the caller's command, in the caller's environment"
        );
        let host = pair.host_db();
        assert!(
            host.get_session_by_id(id).unwrap().is_some(),
            "the host's own row"
        );
        let recipe = host
            .load_launch_recipe(id)
            .unwrap()
            .expect("a command session records its recipe on the host");
        assert_eq!(recipe.command, "sh");
        assert_eq!(
            host.load_reports_as().unwrap().get(&id).map(String::as_str),
            Some("claude"),
            "the declaration belongs where the hooks that read it run"
        );
        let caller = pair.caller_db();
        assert!(
            caller.get_session_by_id(id).unwrap().is_some(),
            "the caller mirrors the host's row under the same id"
        );
        assert_eq!(
            caller
                .load_reports_as()
                .unwrap()
                .get(&id)
                .map(String::as_str),
            Some("claude"),
            "and reads the row against the same agent the host does"
        );
        let log = pair.ssh_log();
        assert!(
            log.contains("session create") && !log.contains("env-secret"),
            "an --env value reached ssh's command line, readable in ps: {log}"
        );
    }

    /// `--resume` reaches the host: the session continues the conversation it
    /// was asked to, rather than starting a fresh one under a new id.
    #[test]
    fn a_delegated_create_resumes_the_conversation_it_was_given() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let out = pair.create(&[
            "--name", "resumed", "--agent", "resumer", "--resume", "conv-7",
        ]);
        let id = created_id(&out);
        let row = pair.host_db().get_session_by_id(id).unwrap().expect("row");
        assert_eq!(row.agent, "resumer");
        assert_eq!(row.agent_session_id.as_deref(), Some("conv-7"));
    }

    #[test]
    fn delegated_registry_args_reach_the_hosts_process() {
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        let capture = pair.path("work/agent-args");
        std::fs::write(pair.path("host-config/agents.toml"),
            "[[agents]]\nname='record'\ncommand='sh'\nargs=['-c', 'printf \"%s\\n\" \"$@\" > \"$CAPTURE\"; exec sleep 600', 'record']\n").unwrap();
        let out = pair.create(&[
            "--name",
            "args",
            "--agent",
            "record",
            "--telemetry-attr",
            "factory.task.id=combined/task",
            "--agent-arg=--model",
            "--agent-arg",
            "literal ; $value",
            "--env",
            &format!("CAPTURE={}", capture.display()),
        ]);
        let id = created_id(&out);
        let expected = vec!["--model".to_string(), "literal ; $value".to_string()];
        assert_eq!(pair.host_db().load_agent_args(id).unwrap(), expected);
        assert_eq!(pair.caller_db().load_agent_args(id).unwrap(), expected);
        assert_eq!(
            pair.host_db().load_telemetry_attrs(id).unwrap()["factory.task.id"],
            "combined/task"
        );
        assert_eq!(wait_for_file(&capture), "--model\nliteral ; $value\n");
    }

    #[test]
    fn delegated_agent_args_refuse_a_peer_without_the_capability() {
        use std::os::unix::fs::PermissionsExt;
        if !super::have_tmux() {
            return;
        }
        let pair = Pair::new();
        std::fs::create_dir(pair.path("host-bin")).unwrap();
        let wrapper = pair.path("host-bin/thurbox-cli");
        std::fs::write(
            &wrapper,
            format!(
                r#"#!/bin/sh
if [ "$1" = version ]; then
  '{}' "$@" | sed 's/"agent_args":[[:space:]]*true/"agent_args":false/g'
else
  exec '{}' "$@"
fi
"#,
                env!("CARGO_BIN_EXE_thurbox-cli"),
                env!("CARGO_BIN_EXE_thurbox-cli")
            ),
        )
        .unwrap();
        std::fs::set_permissions(wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = pair.create(&[
            "--name",
            "unsupported",
            "--agent",
            "resumer",
            "--agent-arg=--opaque",
        ]);
        assert!(!out.status.success());
        let doc = answer(&out);
        assert!(
            doc.to_string().contains("--agent-arg") && doc.to_string().contains("older"),
            "{doc}"
        );
        assert!(pair.host_db().list_active_sessions().unwrap().is_empty());
        assert!(!pair.ssh_log().contains(" session create "));
    }

    /// The link drops after the host created the session: the caller finds it
    /// by the id it minted, rather than reporting a failure that a retry would
    /// turn into a second session.
    #[test]
    fn a_create_whose_answer_was_lost_is_found_by_its_id_not_made_twice() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        std::fs::write(pair.path("drop-answer"), "").unwrap();
        let out = pair.create(&["--name", "dropped"]);
        let id = created_id(&out);
        std::fs::remove_file(pair.path("drop-answer")).unwrap();

        let host = pair.host_db();
        let named: Vec<_> = host
            .find_sessions_by_name("dropped")
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(
            named,
            vec![id],
            "exactly the one session, under the reported id"
        );

        // A driver retrying with the id it was given is answered, not
        // duplicated.
        let again = pair.create(&["--name", "dropped", "--id", &id.to_string()]);
        let doc = answer(&again);
        assert!(again.status.success(), "{doc}");
        assert_eq!(doc["id"], id.to_string());
        assert_eq!(doc["created"], false, "{doc}");
        assert_eq!(host.find_sessions_by_name("dropped").unwrap().len(), 1);
    }

    /// A retry the caller's mirror has not caught up with reaches the host,
    /// which answers its own session under that id — `created: false`, and
    /// before `--on-existing` is asked anything, so even `replace` leaves the
    /// running session exactly as it was.
    #[test]
    fn the_host_answers_a_known_id_whatever_on_existing_says() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let id = SessionId::default();
        let made =
            pair.create_on_host(&["--name", "known", "--key", "k2", "--id", &id.to_string()]);
        assert_eq!(created_id(&made), id);
        let before = pair.host_db().get_session_by_id(id).unwrap().expect("row");
        assert!(
            pair.caller_db().get_session_by_id(id).unwrap().is_none(),
            "the caller has not mirrored it yet"
        );

        // `replace` first, while only the host knows the id; `fail` then meets
        // the caller's own mirror of it.
        for mode in ["replace", "fail"] {
            let out = pair.create(&[
                "--name",
                "known",
                "--key",
                "k2",
                "--id",
                &id.to_string(),
                "--on-existing",
                mode,
            ]);
            let doc = answer(&out);
            assert!(out.status.success(), "{mode}: {doc}");
            assert_eq!(doc["id"], id.to_string(), "{mode}: {doc}");
            assert_eq!(doc["created"], false, "{mode}: {doc}");
        }
        let host = pair.host_db();
        assert_eq!(host.find_sessions_by_name("known").unwrap().len(), 1);
        let after = host.get_session_by_id(id).unwrap().expect("still there");
        assert_eq!(
            after.backend_id, before.backend_id,
            "replace tore down the session it was answered with"
        );
    }

    /// The caller's post-create hooks fire for the sessions this call created
    /// — a lost answer recovered by its id included — and not for one the host
    /// answered as already there under that id.
    #[test]
    fn the_caller_fires_post_create_only_for_a_session_it_created() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let log = pair.path("post-create.log");
        std::fs::write(
            pair.path("config/hooks.toml"),
            format!(
                "[[hooks]]\nevent = \"session.post_create\"\n\
                 command = 'echo \"$THURBOX_SESSION\" >> {}'\n",
                log.display()
            ),
        )
        .unwrap();
        let fired = || std::fs::read_to_string(&log).unwrap_or_default();

        let plain = created_id(&pair.create(&["--name", "plain"]));
        std::fs::write(pair.path("drop-answer"), "").unwrap();
        let recovered = created_id(&pair.create(&["--name", "recovered"]));
        std::fs::remove_file(pair.path("drop-answer")).unwrap();
        assert_eq!(
            fired().lines().collect::<Vec<_>>(),
            [plain.to_string(), recovered.to_string()],
            "one post-create per session this caller created"
        );

        let elsewhere = SessionId::default();
        let made = pair.create_on_host(&["--name", "elsewhere", "--id", &elsewhere.to_string()]);
        assert_eq!(created_id(&made), elsewhere);
        let answered = pair.create(&["--name", "elsewhere", "--id", &elsewhere.to_string()]);
        assert_eq!(answer(&answered)["created"], false);
        assert!(
            !fired().contains(&elsewhere.to_string()),
            "a session the host answered as already there is not this caller's to announce: {}",
            fired()
        );
    }

    /// `--on-existing` is the host's to apply: its rows are the record, and
    /// the caller's mirror may not have seen the session yet. Here the host
    /// already holds the key, the caller has never mirrored it, and `adopt`
    /// must answer with the host's session rather than refuse or make another.
    #[test]
    fn on_existing_is_applied_by_the_host_against_its_own_rows() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let theirs = created_id(&pair.create_on_host(&["--name", "keyed", "--key", "k1"]));
        assert!(
            pair.caller_db()
                .get_session_by_id(theirs)
                .unwrap()
                .is_none(),
            "the caller has not mirrored it yet"
        );

        let out = pair.create(&["--name", "keyed", "--key", "k1", "--on-existing", "adopt"]);
        let doc = answer(&out);
        assert!(out.status.success(), "{doc}");
        assert_eq!(doc["id"], theirs.to_string(), "{doc}");
        assert_eq!(doc["created"], false, "{doc}");
        assert_eq!(
            pair.host_db().find_sessions_by_name("keyed").unwrap().len(),
            1
        );
    }

    /// An adopted session is described as the host found it, not as one that
    /// was just spawned: a driver reads `stopped` to decide whether it still
    /// owes a `session start`.
    #[test]
    fn an_adopted_session_reports_the_hosts_own_state() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let parked = created_id(&pair.create_on_host(&["--name", "parked", "--key", "k3"]));
        let mut stop = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        stop.args(["session", "stop", &parked.to_string(), "--json"])
            .env("HOME", pair.path("host-home"))
            .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
            .env("THURBOX_DATA_DIR", pair.path("host-data"))
            .env_remove("THURBOX_SESSION");
        pair.host_server.scope(&mut stop);
        let stopped = stop.output().expect("run the host's thurbox-cli");
        assert!(
            stopped.status.success(),
            "{}",
            String::from_utf8_lossy(&stopped.stdout)
        );

        let out = pair.create(&["--name", "parked", "--key", "k3", "--on-existing", "adopt"]);
        let doc = answer(&out);
        assert!(out.status.success(), "{doc}");
        assert_eq!(doc["id"], parked.to_string(), "{doc}");
        assert_eq!(doc["created"], false, "{doc}");
        assert_eq!(doc["stopped"], true, "{doc}");
        assert_eq!(doc["state"], "stopped", "{doc}");
    }

    /// A delegated create asks the host, not the caller's mirror, whether the
    /// session is already there: one the host deleted since the last mirror
    /// pass is not adopted back from a stale row.
    #[test]
    fn a_delegated_adopt_does_not_answer_from_a_stale_mirror_row() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        // Both spellings `--host` takes: the gate that hands the decision to
        // the host has to see the host the spawn will delegate to.
        for (n, host) in ["probe", "ssh:probe"].into_iter().enumerate() {
            let pair = Pair::new();
            let key = format!("k4-{n}");
            let first = created_id(&pair.create_via(host, &["--name", "stale", "--key", &key]));
            assert!(pair.caller_db().get_session_by_id(first).unwrap().is_some());
            let mut delete = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
            delete
                .args(["session", "delete", &first.to_string(), "--force", "--json"])
                .env("HOME", pair.path("host-home"))
                .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
                .env("THURBOX_DATA_DIR", pair.path("host-data"))
                .env_remove("THURBOX_SESSION");
            pair.host_server.scope(&mut delete);
            let deleted = delete.output().expect("run the host's thurbox-cli");
            assert!(
                deleted.status.success(),
                "{}",
                String::from_utf8_lossy(&deleted.stdout)
            );
            assert!(
                pair.caller_db().get_session_by_id(first).unwrap().is_some(),
                "the caller's mirror has not caught up"
            );

            let out = pair.create_via(
                host,
                &["--name", "stale", "--key", &key, "--on-existing", "adopt"],
            );
            let doc = answer(&out);
            assert!(out.status.success(), "{host}: {doc}");
            assert_eq!(doc["created"], true, "{host}: {doc}");
            assert_ne!(doc["id"], first.to_string(), "{host}: {doc}");
        }
    }

    /// An id a row here holds for a session on another machine is not the
    /// host's to reuse: the mirror pass after the creation would relabel that
    /// row as the host's, leaving its own pane untracked — or revive a
    /// deleted one as the host's live session.
    #[test]
    fn a_delegated_create_refuses_an_id_this_machine_holds_elsewhere() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let db = pair.caller_db();
        let mine = SessionId::default();
        db.upsert_session(&super::row(mine, "mine", "local-tmux"))
            .unwrap();
        let tombstone = SessionId::default();
        db.upsert_session(&super::row(tombstone, "gone", "local-tmux"))
            .unwrap();
        db.soft_delete_session(tombstone).unwrap();

        for id in [mine, tombstone] {
            let out = pair.create(&["--name", "borrowed", "--id", &id.to_string()]);
            assert_eq!(
                out.status.code(),
                Some(5),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert_eq!(answer(&out)["error_kind"], "conflict");
        }
        assert_eq!(
            db.get_session_by_id(mine).unwrap().unwrap().backend_type,
            "local-tmux",
            "the local session is still this machine's"
        );
        assert!(db.get_session_by_id(tombstone).unwrap().is_none());
        assert!(pair
            .host_db()
            .find_sessions_by_name("borrowed")
            .unwrap()
            .is_empty());
    }

    /// A session the host answers as already there keeps the declaration it
    /// has: the host does not apply a retry's `--reports-as`, so the caller
    /// neither records nor reports one of its own.
    #[test]
    fn a_found_session_keeps_the_hosts_declaration() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let id = SessionId::default();
        let made = pair.create_on_host(&[
            "--name",
            "declared",
            "--id",
            &id.to_string(),
            "--reports-as",
            "claude",
        ]);
        assert_eq!(created_id(&made), id);

        let out = pair.create(&[
            "--name",
            "declared",
            "--id",
            &id.to_string(),
            "--reports-as",
            "codex",
        ]);
        let doc = answer(&out);
        assert!(out.status.success(), "{doc}");
        assert_eq!(doc["created"], false, "{doc}");
        assert_eq!(doc["reports_as"], "claude", "{doc}");
        assert_eq!(
            pair.caller_db()
                .load_reports_as()
                .unwrap()
                .get(&id)
                .map(String::as_str),
            Some("claude")
        );
    }

    /// A retry under an id this machine already mirrors names a session that
    /// was there before the call: when its answer is lost too, finding the id
    /// is not evidence of a creation, so nothing is announced again.
    #[test]
    fn a_lost_answer_to_a_retry_of_a_known_id_is_not_a_new_session() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let log = pair.path("post-create.log");
        std::fs::write(
            pair.path("config/hooks.toml"),
            format!(
                "[[hooks]]\nevent = \"session.post_create\"\n\
                 command = 'echo \"$THURBOX_SESSION\" >> {}'\n",
                log.display()
            ),
        )
        .unwrap();
        let id = created_id(&pair.create(&["--name", "known", "--reports-as", "claude"]));
        assert!(pair.caller_db().get_session_by_id(id).unwrap().is_some());

        std::fs::write(pair.path("drop-answer"), "").unwrap();
        let out = pair.create(&[
            "--name",
            "known",
            "--id",
            &id.to_string(),
            "--reports-as",
            "codex",
        ]);
        std::fs::remove_file(pair.path("drop-answer")).unwrap();
        let doc = answer(&out);
        assert!(out.status.success(), "{doc}");
        assert_eq!(doc["id"], id.to_string(), "{doc}");
        assert_eq!(doc["created"], false, "{doc}");
        // The lost answer is replaced by the host's own `session get`, which
        // reports the declaration the host holds — the retry's was never
        // applied.
        assert_eq!(doc["reports_as"], "claude", "{doc}");
        assert_eq!(
            pair.caller_db()
                .load_reports_as()
                .unwrap()
                .get(&id)
                .map(String::as_str),
            Some("claude")
        );
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "post-create fired again for a session that already existed"
        );
    }

    /// A known-id retry whose answer is lost is described by the host itself,
    /// not by this machine's mirror, which carries no parked mark: a session
    /// parked there must not read as running, or a driver skips its
    /// `session start`.
    #[test]
    fn a_lost_answer_to_a_known_id_retry_reports_a_parked_session_as_parked() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let id = created_id(&pair.create(&["--name", "napping"]));
        let mut stop = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        stop.args(["session", "stop", &id.to_string(), "--json"])
            .env("HOME", pair.path("host-home"))
            .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
            .env("THURBOX_DATA_DIR", pair.path("host-data"))
            .env_remove("THURBOX_SESSION");
        pair.host_server.scope(&mut stop);
        assert!(stop.output().unwrap().status.success());

        std::fs::write(pair.path("drop-answer"), "").unwrap();
        let out = pair.create(&["--name", "napping", "--id", &id.to_string()]);
        std::fs::remove_file(pair.path("drop-answer")).unwrap();
        let doc = answer(&out);
        assert!(out.status.success(), "{doc}");
        assert_eq!(doc["created"], false, "{doc}");
        assert_eq!(doc["stopped"], true, "{doc}");
        assert_eq!(doc["state"], "stopped", "{doc}");
    }

    /// A create that never reached the host is not recovered by a row this
    /// machine still holds for the id: only the host listing the session is
    /// evidence that it exists.
    #[test]
    fn a_create_that_never_arrived_is_not_recovered_from_a_stale_row() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let id = SessionId::default();
        pair.caller_db()
            .upsert_session(&super::row(id, "ghost", "ssh:probe:tmux"))
            .unwrap();

        std::fs::write(pair.path("never-arrives"), "").unwrap();
        let out = pair.create(&["--name", "ghost", "--id", &id.to_string()]);
        std::fs::remove_file(pair.path("never-arrives")).unwrap();
        assert!(
            !out.status.success(),
            "reported a session the host never made: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        // The one thing a driver needs to retry without making a second
        // session is the id, so the error names it.
        let doc = answer(&out);
        assert!(
            doc["error"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!("retry with `--id {id}`")),
            "{doc}"
        );
        assert!(pair.host_db().get_session_by_id(id).unwrap().is_none());
    }

    /// A recovered session is described by the host however it was found,
    /// including under an id this machine had not mirrored: one already parked
    /// there must not read as freshly running.
    #[test]
    fn a_lost_answer_for_an_unmirrored_id_reports_a_parked_session_as_parked() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let id = SessionId::default();
        assert_eq!(
            created_id(&pair.create_on_host(&["--name", "dozing", "--id", &id.to_string()])),
            id
        );
        let mut stop = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        stop.args(["session", "stop", &id.to_string(), "--json"])
            .env("HOME", pair.path("host-home"))
            .env("THURBOX_CONFIG_DIR", pair.path("host-config"))
            .env("THURBOX_DATA_DIR", pair.path("host-data"))
            .env_remove("THURBOX_SESSION");
        pair.host_server.scope(&mut stop);
        assert!(stop.output().unwrap().status.success());
        assert!(pair.caller_db().get_session_by_id(id).unwrap().is_none());

        std::fs::write(pair.path("drop-answer"), "").unwrap();
        let out = pair.create(&["--name", "dozing", "--id", &id.to_string()]);
        std::fs::remove_file(pair.path("drop-answer")).unwrap();
        let doc = answer(&out);
        assert!(out.status.success(), "{doc}");
        assert_eq!(doc["id"], id.to_string(), "{doc}");
        assert_eq!(doc["stopped"], true, "{doc}");
        assert_eq!(doc["state"], "stopped", "{doc}");
    }

    /// Recovery takes the host's word only for the session it asked about: an
    /// answer naming another id is no evidence that this one exists.
    #[test]
    fn a_lookup_answered_for_another_session_is_not_a_recovery() {
        if !super::have_tmux() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
        let pair = Pair::new();
        let id = SessionId::default();
        std::fs::write(pair.path("never-arrives"), "").unwrap();
        std::fs::write(pair.path("get-answers-another"), "").unwrap();
        let out = pair.create(&["--name", "elsewhere", "--id", &id.to_string()]);
        std::fs::remove_file(pair.path("never-arrives")).unwrap();
        std::fs::remove_file(pair.path("get-answers-another")).unwrap();
        assert!(
            !out.status.success(),
            "recovered on an answer about another session: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        // Not taken for a recovery that then went wrong: the answer is the
        // unanswered question's, naming the id a retry needs.
        let doc = answer(&out);
        assert!(
            doc["error"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!("retry with `--id {id}`")),
            "{doc}"
        );
    }
}

#[test]
fn a_purge_tombstone_learned_before_the_live_row_blocks_stale_adoption() {
    let temp = tempfile::tempdir().unwrap();
    let _guard = thurbox::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    let id = SessionId::default();
    let purged = mirror::parse_deleted(
        &json!([{"id": id.to_string(), "force_deleted": true, "purged_at": 1}]),
    );
    mirror::apply(&db, BACKEND, &[], &purged);
    assert!(db.list_deleted_sessions().unwrap().is_empty());
    assert_eq!(db.list_session_tombstones(None).unwrap().len(), 1);
    db.upsert_session(&row(id, "stale", BACKEND)).unwrap();
    assert!(db.get_session_by_id(id).unwrap().is_none());
    db.restore_session(id).unwrap();
    assert!(db.get_session_by_id(id).unwrap().is_none());
}

#[test]
fn a_remote_purge_cannot_touch_a_different_routes_deleted_row_with_the_same_id() {
    let temp = tempfile::tempdir().unwrap();
    let _guard = thurbox::paths::TestPathGuard::new(temp.path());
    let db = Database::open_in_memory().unwrap();
    let local = row(SessionId::default(), "local", "local-tmux");
    db.upsert_session(&local).unwrap();
    db.force_delete_session(local.id).unwrap();
    db.set_session_meta(local.id, "keep", "value").unwrap();
    let gone = mirror::parse_deleted(
        &json!([{"id": local.id.to_string(), "force_deleted": true, "purged_at": 1}]),
    );
    mirror::apply(&db, BACKEND, &[], &gone);
    let kept = db
        .get_deleted_session_by_id(local.id)
        .unwrap()
        .expect("foreign purge erased local deletion");
    assert!(
        kept.teardown_owed,
        "foreign purge cleared local cleanup debt"
    );
    assert_eq!(
        db.get_session_meta(local.id, "keep").unwrap().as_deref(),
        Some("value")
    );
}
