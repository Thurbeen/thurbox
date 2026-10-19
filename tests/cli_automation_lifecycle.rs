//! What the automation verbs leave on the row, driven through the real binary.
//!
//! Each test here pins one place where a verb changed more, or less, of an
//! automation than it says it does: a delete that left a send armed against a
//! session that is gone, a run that switched a disabled automation back on, a
//! run row that only appeared once the run was over, and an edit that wrote
//! the whole row back from what it had read.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::sync::SharedSession;

/// A throwaway thurbox instance with the heartbeat off: nothing fires here
/// except the tick a test runs itself.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "tmux"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        std::fs::write(
            root.path().join("config/settings.toml"),
            "[features]\nautomations = false\nversion_check = false\nauto_update = false\n",
        )
        .expect("settings");
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.arg("--json").args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env("TMUX_TMPDIR", self.path("tmux"));
        cmd.env_remove("THURBOX_SOCKET");
        cmd.env_remove("THURBOX_SOCKET_FOR");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        cmd.output().expect("run thurbox-cli")
    }

    /// Run `args`, assert it succeeded, and return its JSON document.
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("JSON")
    }

    fn automation(&self, id: &str) -> Value {
        self.ok(&["automation", "show", id])
    }

    /// A session row with no window: the verbs under test act on rows.
    fn seed_session(&self, name: &str) -> String {
        let db = thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database");
        let row = SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "claude".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        };
        db.upsert_session(&row).expect("persist");
        row.id.to_string()
    }
}

fn id_of(doc: &Value) -> String {
    doc["id"].as_i64().expect("an automation id").to_string()
}

/// A soft delete disables the send automations aimed at the session, as a
/// force delete and a delete delegated to a shared host already did. Left
/// armed, the next fire found no session and recorded a skip every hour.
#[test]
fn a_soft_delete_disables_the_send_automations_of_its_session() {
    let env = Env::new();
    let session = env.seed_session("worker");
    let auto = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "nudge",
        "--trigger",
        "hourly",
        "--prompt",
        "continue",
        "--session",
        &session,
    ]));
    assert_eq!(env.automation(&auto)["enabled"], true);

    let report = env.ok(&["session", "delete", &session, "--soft"]);
    assert_eq!(report["disabled_automations"], 1, "{report}");

    let after = env.automation(&auto);
    assert_eq!(after["enabled"], false, "{after}");
    assert_eq!(after["next_run_at"], Value::Null, "{after}");
}

/// `automation run` fires an automation; it does not switch one on. Marking a
/// disabled one due used to enable it as a side effect, so it went on firing
/// on its schedule after the one run that was asked for.
#[test]
fn running_a_disabled_automation_leaves_it_disabled() {
    let env = Env::new();
    let auto = id_of(&env.ok(&[
        "automation",
        "create",
        "--name",
        "paused",
        "--trigger",
        "hourly",
        "--command",
        "true",
        "--disabled",
    ]));

    let out = env.run(&["automation", "run", &auto]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a disabled automation is refused, not fired: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let after = env.automation(&auto);
    assert_eq!(after["enabled"], false, "{after}");
    assert_eq!(after["next_run_at"], Value::Null, "{after}");
}
