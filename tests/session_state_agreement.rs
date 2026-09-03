//! One row, four surfaces, one word.
//!
//! `state` is answered by `session get`, `session list`, `watch` and the
//! interface's own snapshot, and each used to derive it from the same three
//! hook columns in its own way. The concrete divergence this pins: `seen_at`
//! is a *stored fact* — the interface stamps it when the user moves focus off
//! a finished turn — and only the snapshot ever read it, so a turn the TUI had
//! already acknowledged as `idle` stayed `done` on every headless surface for
//! the rest of the session's life.
//!
//! Driven through the real binary for the three CLI surfaces and through
//! `SnapshotStore` for the fourth, because the property under test is that
//! four *independent* readers of one database row agree.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// `claude` reports every state, so nothing here is answered by a coverage gap.
const AGENTS_TOML: &str = r#"
config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"
"#;

/// A throwaway instance whose config and data share one directory — the layout
/// `paths::TestPathGuard` imposes in-process, so the subprocess surfaces and
/// the in-process one read the very same files.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(root.path().join("agents.toml"), AGENTS_TOML).expect("write agents.toml");
        Self { root }
    }

    fn base(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    fn db(&self) -> Database {
        Database::open(&self.base().join("thurbox.db")).expect("open the instance database")
    }

    fn cli(&self, args: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(args)
            .env("HOME", self.base())
            .env("USERPROFILE", self.base())
            .env("THURBOX_CONFIG_DIR", self.base())
            .env("THURBOX_DATA_DIR", self.base())
            // Named outright: a relocated data dir derives a socket of its own,
            // and nothing here may reach the operator's server by accident.
            .env("THURBOX_SOCKET", "thurbox-agreement-test")
            .env_remove("THURBOX_SOCKET_FOR")
            .env_remove("THURBOX_SESSION")
            .output()
            .expect("run thurbox-cli");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "thurbox-cli {args:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The stream commands print one JSON document per line, the rest print
        // exactly one — so the first document is the answer in both shapes.
        serde_json::Deserializer::from_str(&stdout)
            .into_iter::<Value>()
            .next()
            .and_then(Result::ok)
            .unwrap_or_else(|| panic!("{args:?} printed no JSON:\n{stdout}"))
    }
}

fn seed(db: &Database, name: &str) -> SessionId {
    let row = SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "claude".into(),
        backend_id: "%7".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: Some("sid".into()),
        cwd: Some(PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    db.upsert_session(&row).expect("upsert");
    row.id
}

/// What each of the four surfaces answers for `id`, in one call.
fn states(env: &Env, id: SessionId) -> [String; 4] {
    let get = env.cli(&["--json", "session", "get", &id.to_string()]);
    let list = env.cli(&["--json", "session", "list"]);
    let watch = env.cli(&["--json", "watch", "--initial", "--for-secs", "1"]);

    let _guard = thurbox::paths::TestPathGuard::new(env.base());
    let mut store = SnapshotStore::with_database(env.db());
    store.refresh();
    let tui = store
        .current()
        .sessions
        .iter()
        .find(|row| row.id == id.to_string())
        .map(|row| row.status.as_str().to_string())
        .expect("the session is in the snapshot");

    [
        word(&get),
        word(
            list.as_array()
                .expect("session list is an array")
                .iter()
                .find(|row| row["id"] == json_id(id))
                .expect("the session is in the listing"),
        ),
        word(&watch),
        tui,
    ]
}

fn json_id(id: SessionId) -> Value {
    Value::String(id.to_string())
}

fn word(row: &Value) -> String {
    row["state"]
        .as_str()
        .unwrap_or_else(|| panic!("no state in {row}"))
        .to_string()
}

/// The regression. The interface stamped `seen_at` when the user looked away
/// from a finished turn; every headless surface went on reporting `done`.
#[test]
fn an_acknowledged_done_is_idle_on_every_surface() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "acknowledged");
    db.set_hook_state(id, "done").expect("signal");
    let at = db
        .load_hook_state(id)
        .expect("read back")
        .expect("row")
        .state_at
        .expect("stamped");
    db.mark_session_seen(id, at).expect("acknowledge");

    let states = states(&env, id);
    assert_eq!(
        states,
        ["idle", "idle", "idle", "idle"],
        "get, list, watch, tui"
    );
}

/// The other half of the same rule: an unacknowledged finish is `done`
/// everywhere, so the fold above cannot be a blanket downgrade.
#[test]
fn an_unacknowledged_done_is_done_on_every_surface() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "finished");
    db.set_hook_state(id, "done").expect("signal");

    let states = states(&env, id);
    assert_eq!(
        states,
        ["done", "done", "done", "done"],
        "get, list, watch, tui"
    );
}

/// A standing request for input is never folded away by anything.
#[test]
fn a_blocked_session_is_blocked_on_every_surface() {
    let env = Env::new();
    let db = env.db();
    let id = seed(&db, "waiting");
    db.set_hook_state(id, "blocked").expect("signal");

    let states = states(&env, id);
    assert_eq!(
        states,
        ["blocked", "blocked", "blocked", "blocked"],
        "get, list, watch, tui"
    );
}
