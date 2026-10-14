//! The shared-session mirror over a real `ssh` round trip: an idle host is
//! not re-listed.
//!
//! The mirror runs every few seconds per host, and a host's full `session
//! list` + `session list --deleted` grows with its deleted history — on a long
//! lived host, over a hundred kilobytes a pass while nothing happened. Here an
//! `ssh` stand-in on `PATH` runs each remote script through `sh` against a
//! second data directory with the real `thurbox-cli`, logging every script, so
//! the test sees exactly what crossed the wire.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thurbox::session::{HostDef, SessionId};
use thurbox::session_ops::host_cli::{self, Usable};
use thurbox::session_ops::mirror;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

fn row(id: SessionId, name: &str) -> SharedSession {
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
    }
}

/// An `ssh` that drops its options and destination and runs the remote
/// command with `sh`, as the host's login shell would, inside the host's own
/// thurbox directories.
fn write_ssh(bin: &Path, root: &Path) {
    let cli_dir = Path::new(env!("CARGO_BIN_EXE_thurbox-cli"))
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
export PATH='{cli_dir}':/usr/bin:/bin
exec sh -c "$*"
"#,
        root = root.display(),
    );
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, script).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// How many full listings have crossed the wire so far.
fn listings(root: &Path) -> usize {
    std::fs::read_to_string(root.join("ssh.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.replace('\'', "").contains("session list"))
        .count()
}

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
    host_db.upsert_session(&row(live, "live")).unwrap();
    for n in 0..50 {
        let gone = SessionId::default();
        host_db
            .upsert_session(&row(gone, &format!("gone-{n}")))
            .unwrap();
        host_db.soft_delete_session(gone).unwrap();
    }

    let host = HostDef {
        name: "probe".into(),
        destination: "probe".into(),
        ..Default::default()
    };
    let cli = match host_cli::usable(&host) {
        Usable::Yes(cli) => cli,
        Usable::No(why) => panic!("the stand-in host is not usable: {why}"),
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
