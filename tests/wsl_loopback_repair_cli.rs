//! The one-time WSL row repair runs from `thurbox-cli`, not only from the TUI.
//!
//! Schema v47 marks the repair as owed on whichever binary opens the database
//! first, and a headless-driven install (an automation, the heartbeat keeper,
//! an agent's status hook) need never launch the interface. Until the repair
//! runs, a session local to the distro reads as remote — `is_remote_backend`
//! is true of `wsl:<us>` — so a reap sweep refuses to kill its windows and
//! leaks the agent process.
//!
//! Driven through the real binary, because the thing under test is the
//! entrypoint's own wiring: nothing below `main` can observe whether the CLI
//! ever calls the repair.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

/// A throwaway thurbox instance, pretending to run inside WSL distro
/// `MagicDebian`: its own config, data and home, so nothing here reads or
/// writes the operator's.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db_path(&self) -> PathBuf {
        self.path("data").join("thurbox.db")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env("WSL_DISTRO_NAME", "MagicDebian");
        cmd.env_remove("THURBOX_SOCKET");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        cmd.output().expect("run thurbox-cli")
    }
}

/// Roll the database back to v46 and plant one session recorded the way the
/// released build recorded it: local to this distro, but stamped with the
/// distro's own backend name. The next open migrates to v47, which marks the
/// repair owed — exactly the state an upgrading user is in.
fn plant_a_relabelled_session(db: &PathBuf, backend_type: &str) {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.execute(
        "UPDATE metadata SET value = '46' WHERE key = 'schema_version'",
        [],
    )
    .expect("downgrade");
    conn.execute(
        "INSERT INTO sessions (id, name, agent, backend_type, backend_id, created_at, \
         updated_at) VALUES ('11111111-1111-4111-8111-111111111111', 'relabelled', \
         'claude', ?1, '%1', 0, 0)",
        [backend_type],
    )
    .expect("insert session");
}

fn sessions(out: &Output) -> Vec<Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "session list succeeded:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    match serde_json::from_slice(&out.stdout).expect("stdout is JSON") {
        Value::Array(rows) => rows,
        other => panic!("session list returns an array, got {other}"),
    }
}

fn backend_of(rows: &[Value], name: &str) -> String {
    rows.iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("session '{name}' is listed: {rows:?}"))["backend_type"]
        .as_str()
        .expect("backend_type is a string")
        .to_string()
}

#[test]
fn a_cli_invocation_repairs_the_rows_a_loopback_host_relabelled() {
    let env = Env::new();
    // First run creates the database at the current schema.
    assert!(env.run(&["session", "list", "--json"]).status.success());
    plant_a_relabelled_session(&env.db_path(), "wsl:MagicDebian");

    // No TUI in sight: the next CLI invocation is what has to put it back.
    let rows = sessions(&env.run(&["session", "list", "--json"]));

    assert_eq!(
        backend_of(&rows, "relabelled"),
        "local-tmux",
        "a session on the distro thurbox runs in is local to it"
    );
}

#[test]
fn a_cli_invocation_leaves_a_sibling_distros_sessions_remote() {
    let env = Env::new();
    assert!(env.run(&["session", "list", "--json"]).status.success());
    plant_a_relabelled_session(&env.db_path(), "wsl:MagicDebianPerso");

    let rows = sessions(&env.run(&["session", "list", "--json"]));

    assert_eq!(
        backend_of(&rows, "relabelled"),
        "wsl:MagicDebianPerso",
        "reaching a sibling from inside one distro is an ordinary remote session"
    );
}

/// The rows a host *named* after the current distro wrote are remote — they
/// run on the sibling it reaches — so the repair moves them onto that distro's
/// backend name rather than relabelling them local, and the host follows.
#[test]
fn a_cli_invocation_moves_a_shadow_hosts_sessions_onto_its_real_distro() {
    let env = Env::new();
    assert!(env.run(&["session", "list", "--json"]).status.success());
    std::fs::write(
        env.path("config").join("hosts.toml"),
        "[[hosts]]\nname = \"MagicDebian\"\nkind = \"wsl\"\ndistro = \"MagicDebianPerso\"\n",
    )
    .expect("write hosts.toml");
    plant_a_relabelled_session(&env.db_path(), "wsl:MagicDebian");

    let rows = sessions(&env.run(&["session", "list", "--json"]));

    assert_eq!(
        backend_of(&rows, "relabelled"),
        "wsl:MagicDebianPerso",
        "the entry reached a sibling, so its sessions are that sibling's"
    );
}
