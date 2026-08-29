//! Stdout carries exactly one document per invocation.
//!
//! Two commands render a full report and *then* ask for a non-zero exit:
//! `session doctor` on a session whose hooks cannot fire, and `config validate`
//! on a file that does not parse. Both are commands an integrator scripts, and
//! both are read by a single-document parser (`jq`, `serde_json::from_slice`) —
//! so a second structured error appended after the report is not a cosmetic
//! duplicate, it is a parse failure on exactly the answers that matter.
//!
//! Driven through the real binary, because the thing under test is what lands
//! on the process's streams: nothing below `main` can observe the second
//! `println!` that used to follow the first.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use thurbox::session::SessionId;
use thurbox::sync::SharedSession;

/// A throwaway thurbox instance: its own config, data and home, so no test here
/// reads or writes the operator's.
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

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(args);
        cmd.env("HOME", self.path("home"));
        cmd.env("USERPROFILE", self.path("home"));
        cmd.env("THURBOX_CONFIG_DIR", self.path("config"));
        cmd.env("THURBOX_DATA_DIR", self.path("data"));
        cmd.env_remove("THURBOX_SOCKET");
        cmd.env_remove("THURBOX_SESSION");
        cmd.env_remove("THURBOX_SESSION_ID");
        cmd.output().expect("run thurbox-cli")
    }
}

/// Assert `out` failed with the "command ran and failed" status and that its
/// stdout is one JSON value and nothing else, returning that value.
///
/// `from_slice` alone would accept a leading document and ignore the rest, so
/// the stream is read through a streaming deserializer and then asserted to be
/// exhausted — which is precisely the distinction a `jq` consumer trips over.
fn sole_document(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the command ran and failed:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut stream = serde_json::Deserializer::from_slice(&out.stdout).into_iter::<Value>();
    let first = stream
        .next()
        .unwrap_or_else(|| panic!("stdout carries a document: {stdout}"))
        .unwrap_or_else(|e| panic!("stdout is JSON ({e}): {stdout}"));
    assert!(
        stream.next().is_none(),
        "stdout must carry exactly one document, so a single-document parser \
         can read it:\n{stdout}"
    );
    first
}

/// Seed a session row into the instance's own database.
///
/// Written through `storage::Database` rather than `session register`, which
/// refuses a session with no live window — and a window is exactly what this
/// test must not create. `doctor` needs a *row*, not a pane; the pane check
/// answers "no live pane" and is not what is under test here.
fn seed_session(env: &Env, name: &str, agent: &str) -> String {
    let db = thurbox::storage::Database::open(&env.path("data").join("thurbox.db"))
        .expect("open the instance database");
    let row = SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: agent.into(),
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
    };
    db.upsert_session(&row).expect("persist");
    row.id.to_string()
}

#[test]
fn session_doctor_on_a_broken_session_prints_one_document_and_exits_non_zero() {
    let env = Env::new();
    // A scratch config has no hooks installed, so claude's payload is missing
    // and the verdict is `fail` — the shape that renders a report *and* asks
    // for a non-zero exit.
    let id = seed_session(&env, "unwired", "claude");

    let out = env.run(&["session", "doctor", &id, "--json"]);
    let doc = sole_document(&out);

    let report = doc.as_array().expect("one report per session")[0].clone();
    assert_eq!(report["verdict"], Value::String("fail".into()), "{report}");
    assert_eq!(report["session_name"], Value::String("unwired".into()));
    // The verdict still has to be explained; it goes where it cannot corrupt
    // the answer.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unwired"),
        "the failing session is named on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn config_validate_on_an_invalid_file_prints_one_document_and_exits_non_zero() {
    let env = Env::new();
    let config = env.path("config");
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::write(config.join("agents.toml"), "this is not = = toml\n").expect("write");

    let out = env.run(&["config", "validate", "--json"]);
    let doc = sole_document(&out);

    assert_eq!(doc["valid"], Value::Bool(false), "{doc}");
    assert_eq!(doc["agents_toml"]["valid"], Value::Bool(false), "{doc}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("config invalid"),
        "the reason for the exit code is stated, on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A failure that happened while the command *ran* must not be advised as a bad
/// invocation. The two kinds already exit differently (1 vs 2) and clap owns the
/// usage wording; a runtime failure that also answered "check the arguments" and
/// pointed at `--help` sends an agent to a page that cannot fix "no such
/// session".
#[test]
fn a_runtime_failure_is_not_advised_as_a_usage_error() {
    let env = Env::new();
    // A well-formed UUID no row carries: the invocation is correct, the world
    // is not what it asked for.
    let missing = "00000000-0000-4000-8000-00000000dead";

    let json = env.run(&["session", "get", missing, "--json"]);
    let doc = sole_document(&json);
    assert!(
        doc["error"].as_str().unwrap_or_default().contains(missing),
        "the message names what was not found: {doc}"
    );
    let suggestion = doc["suggestion"].as_str().expect("a suggestion").to_string();
    assert!(
        !suggestion.contains("argument"),
        "a runtime failure does not blame the arguments: {suggestion}"
    );

    // The runnable next step only renders outside `--json`, and it is the half
    // that pointed at the usage page.
    let toon = env.run(&["session", "get", missing, "--toon"]);
    let rendered = String::from_utf8_lossy(&toon.stdout).to_string();
    assert_eq!(toon.status.code(), Some(1), "{rendered}");
    assert!(
        !rendered.contains("--help"),
        "the next step for a runtime failure is not the usage page: {rendered}"
    );

    // The genuine usage error — a missing required argument — still is, and
    // says so with its own exit code.
    let usage = env.run(&["session", "get", "--toon"]);
    assert_eq!(
        usage.status.code(),
        Some(2),
        "a bad invocation exits 2: {}",
        String::from_utf8_lossy(&usage.stdout)
    );
    assert!(
        String::from_utf8_lossy(&usage.stdout).contains("--help"),
        "a bad invocation is the one that is sent to the usage page: {}",
        String::from_utf8_lossy(&usage.stdout)
    );
}
