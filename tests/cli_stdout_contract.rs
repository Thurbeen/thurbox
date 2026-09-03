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
/// [`seed_session`] with a working directory, for the verbs that run something
/// in it rather than merely reporting on the row.
fn seed_session_in(env: &Env, name: &str, cwd: Option<&std::path::Path>) -> String {
    let id = seed_session(env, name, "claude");
    if let Some(dir) = cwd {
        let db = thurbox::storage::Database::open(&env.path("data").join("thurbox.db"))
            .expect("open the instance database");
        let parsed: SessionId = id.parse().expect("seeded id");
        let mut row = db
            .get_session_by_id(parsed)
            .expect("query")
            .expect("just seeded");
        row.cwd = Some(dir.to_path_buf());
        db.upsert_session(&row).expect("record the cwd");
    }
    id
}

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

/// Open the instance database directly, for the columns no verb sets from
/// outside a real spawn.
fn open_db(env: &Env) -> thurbox::storage::Database {
    thurbox::storage::Database::open(&env.path("data").join("thurbox.db"))
        .expect("open the instance database")
}

/// A `--command` session: the shape thurbox advertises for drivers (firstmate
/// creates every task as `--command $SHELL --arg -i`), named after the
/// command's file stem and with the launch recipe that makes it one.
fn seed_command_session(env: &Env, name: &str) -> String {
    let id = seed_session(env, name, "bash");
    let db = open_db(env);
    db.set_launch_recipe(
        id.parse().expect("seeded id"),
        &thurbox::session::LaunchRecipe {
            command: "/bin/bash".into(),
            args: vec!["-i".into()],
            env: Default::default(),
        },
    )
    .expect("record the launch recipe");
    id
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
    // The half of the contract a driver has to read the other way round: an
    // `error` key implies a non-zero exit, never the converse. A supervisor
    // that gates on `$?` before parsing throws away every diagnosis it will
    // ever ask for — so the document it discarded must be shown to be a
    // *report*, not an error.
    assert!(
        report.get("error").is_none() && doc.get("error").is_none(),
        "a failed doctor run is a report, not an error document: {doc}"
    );
}

/// `doctor` must not fail a session thurbox never wired an agent for.
///
/// A `--command` session is by construction uncovered and, until something
/// types an agent into it, unreported — so the old `Coverage::None` + not
/// reported => Fail turned the exact session shape thurbox advertises for
/// drivers into "hook wiring is broken". Worse, bare `session doctor`
/// diagnoses every active session, so one shell session failed the whole
/// machine.
#[test]
fn session_doctor_expects_no_hooks_from_a_command_session() {
    let env = Env::new();
    seed_command_session(&env, "task-7");

    for args in [
        vec!["session", "doctor", "task-7", "--json"],
        vec!["session", "doctor", "--json"],
    ] {
        let out = env.run(&args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?} must not fail a session with no agent to wire:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
        let report = doc.as_array().expect("one report per session")[0].clone();
        assert_ne!(report["verdict"], Value::String("fail".into()), "{report}");
    }
}

/// The row has to be able to say "this pane runs claude".
///
/// A driver that applied `agent launch-args claude` inside a `--command bash`
/// session left thurbox reading coverage against `bash`: `hook_coverage:
/// "none"`, no reportable states, and `hook_blocked_is_heuristic: false` —
/// asserting the block signal is structured when it is claude's text match on a
/// notification body, which is the single caveat a supervisor most needs.
#[test]
fn a_declared_agent_is_what_coverage_is_published_against() {
    let env = Env::new();
    seed_command_session(&env, "task-7");

    let before: Value = serde_json::from_slice(
        &env.run(&["session", "get", "task-7", "--no-verify", "--json"])
            .stdout,
    )
    .expect("JSON");
    assert_eq!(before["hook_coverage"], Value::String("none".into()));
    assert_eq!(before["hook_blocked_is_heuristic"], Value::Bool(false));

    let declared = env.run(&["session", "reports-as", "task-7", "claude", "--json"]);
    assert_eq!(
        declared.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&declared.stderr)
    );

    let after: Value = serde_json::from_slice(
        &env.run(&["session", "get", "task-7", "--no-verify", "--json"])
            .stdout,
    )
    .expect("JSON");
    assert_eq!(
        after["hook_coverage"],
        Value::String("full".into()),
        "{after}"
    );
    assert_eq!(after["hook_blocked_is_heuristic"], Value::Bool(true));
    // The row still runs what it always ran; only what reports changed.
    assert_eq!(after["agent"], Value::String("bash".into()));

    // A typo is refused rather than silently recording a declaration that
    // unlocks nothing.
    let typo = env.run(&["session", "reports-as", "task-7", "clyde", "--json"]);
    assert_eq!(typo.status.code(), Some(1));
}

/// `session restore` takes a reference like every other session verb.
///
/// It was the one that did not: a raw UUID, error `Invalid session UUID`. A
/// driver holding a name had to keep its own name-to-id map for that one verb.
#[test]
fn session_restore_takes_a_reference_like_every_other_verb() {
    let env = Env::new();
    let id = seed_session(&env, "gone", "claude");
    assert_eq!(
        env.run(&["session", "delete", &id, "--json"]).status.code(),
        Some(0)
    );

    let out = env.run(&["session", "restore", "gone", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "restore by name: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(doc["id"], Value::String(id.clone()));

    // And a reference that matches nothing still says what it tried.
    let missing = env.run(&["session", "restore", "never-existed", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
}

/// A reference matching two sessions is a different answer from one matching
/// none, and a driver has to be able to tell them apart without matching on the
/// message: it reconciles the first by creating a session, and can only escalate
/// the second.
#[test]
fn an_ambiguous_reference_exits_differently_from_a_missing_one() {
    let env = Env::new();
    seed_session(&env, "twin", "claude");
    seed_session(&env, "twin", "codex");

    let ambiguous = env.run(&["session", "get", "twin", "--json"]);
    assert_eq!(
        ambiguous.status.code(),
        Some(3),
        "stdout: {}",
        String::from_utf8_lossy(&ambiguous.stdout)
    );
    let doc: Value = serde_json::from_slice(&ambiguous.stdout).expect("JSON");
    assert!(
        doc["error"].as_str().is_some_and(|e| e.contains("twin")),
        "{doc}"
    );

    assert_eq!(
        env.run(&["session", "get", "nope", "--json"]).status.code(),
        Some(1)
    );
}

/// `replace` is a force delete followed by a create, and it cannot be the other
/// way round: the new session wants the branch and the checkout the old one
/// holds. So a spawn that fails after the teardown used to leave the caller
/// with neither session — the mode's own help said "tear the existing session
/// down first", and the skill's "a refusal leaves no window, worktree or row
/// behind" read as a safety claim `replace` did not honour.
#[test]
fn replace_puts_the_old_session_back_when_the_replacement_cannot_spawn() {
    let env = Env::new();
    let id = seed_session(&env, "worker", "claude");

    // A worktree branch off a directory that is not a git repository: the
    // spawn fails after the teardown and before any window exists, which is
    // exactly the window this rollback covers.
    let out = env.run(&[
        "session",
        "create",
        "--name",
        "worker",
        "--repo-path",
        env.path("home").to_str().expect("utf-8 path"),
        "--worktree-branch",
        "feat",
        "--on-existing",
        "replace",
        "--json",
    ]);
    assert_ne!(out.status.code(), Some(0), "the spawn must have failed");
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let error = doc["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("restored"),
        "the answer says what became of the session it replaced: {doc}"
    );

    // The row is back, and addressable — not left as a tombstone.
    let listed = env.run(&["session", "list", "--json"]);
    let rows: Value = serde_json::from_slice(&listed.stdout).expect("JSON");
    assert!(
        rows.as_array()
            .expect("array")
            .iter()
            .any(|r| r["id"] == Value::String(id.clone())),
        "the replaced session came back: {rows}"
    );
}

/// `--on-existing adopt` exists so a reconciling driver can skip the follow-up
/// read. A **parked** session — no pane, every `send`/`key`/`capture` refused —
/// came back with nothing in the answer saying so.
#[test]
fn adopt_says_when_the_session_it_hands_back_has_no_pane() {
    let env = Env::new();
    let repo = env.path("home");
    let id = seed_session(&env, "worker", "claude");
    open_db(&env)
        .set_session_stopped(id.parse().expect("seeded id"), true)
        .expect("park it");

    let out = env.run(&[
        "session",
        "create",
        "--name",
        "worker",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--on-existing",
        "adopt",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(doc["id"], Value::String(id));
    assert_eq!(doc["created"], Value::Bool(false));
    assert_eq!(doc["stopped"], Value::Bool(true), "{doc}");
    assert_eq!(doc["state"], Value::String("stopped".into()), "{doc}");
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
    let suggestion = doc["suggestion"]
        .as_str()
        .expect("a suggestion")
        .to_string();
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

/// `session exec --exit-passthrough` exits with the command's own code.
///
/// The flag's whole purpose is Gas City's `proc.exec` capability: "the exec
/// op's process exit code carries the in-box command's exit code, so an
/// exec-op exit of 2 is read as the command's own exit 2 rather than the
/// unknown-op sentinel". Collapsing every failure to 1 makes that unreadable —
/// and a caller that trusted the flag's own help would mis-report every
/// non-zero code as 1.
///
/// The single-document rule still applies: the report is on stdout either way,
/// so a caller never has to choose between reading the answer and knowing the
/// result.
#[test]
fn exec_exit_passthrough_carries_the_commands_own_code() {
    let env = Env::new();
    let dir = env.path("home");
    let id = seed_session_in(&env, "worker", Some(&dir));

    // Without the flag: the command failed, the invocation did not. Exit 0,
    // because thurbox was asked to run something and ran it.
    let plain = env.run(&["session", "exec", "worker", "--", "sh", "-c", "exit 7"]);
    assert_eq!(
        plain.status.code(),
        Some(0),
        "an unasked-for failure is data"
    );

    // With it: the command's code *is* the invocation's.
    for code in [7, 3] {
        let out = env.run(&[
            "session",
            "exec",
            "--exit-passthrough",
            "--json",
            &id,
            "--",
            "sh",
            "-c",
            &format!("exit {code}"),
        ]);
        assert_eq!(
            out.status.code(),
            Some(code),
            "--exit-passthrough must carry {code}, not collapse it:\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // And the report is still exactly one document on stdout.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut stream = serde_json::Deserializer::from_slice(&out.stdout).into_iter::<Value>();
        let first = stream
            .next()
            .unwrap_or_else(|| panic!("stdout carries a document: {stdout}"))
            .expect("stdout is JSON");
        assert!(stream.next().is_none(), "one document only:\n{stdout}");
        assert_eq!(first["exit_code"].as_i64(), Some(i64::from(code)));
    }

    // A command that succeeds exits 0 with the flag, like any other success.
    let ok = env.run(&[
        "session",
        "exec",
        "--exit-passthrough",
        "worker",
        "--",
        "sh",
        "-c",
        "exit 0",
    ]);
    assert_eq!(ok.status.code(), Some(0));
}

/// `session create` answers the "a session of this name already exists"
/// question four ways, and `fail` is the one that was missing.
///
/// Both orchestrators tested against this branch hand-rolled the same
/// duplicate refusal, each with its own list-then-create race, because thurbox
/// offered adopt and replace but no way to *refuse*. Gas City's
/// `RPP-LIFECYCLE-002` mandates that a duplicate start exit non-zero, so the
/// exit code is part of the contract, not decoration.
///
/// No multiplexer is involved: every arm here is decided before anything is
/// spawned, which is also why it can refuse without leaving a window behind.
#[test]
fn create_answers_an_existing_name_four_ways() {
    let env = Env::new();
    let repo = env.path("home");
    let existing = seed_session(&env, "worker", "claude");

    let create = |mode: &str| {
        env.run(&[
            "session",
            "create",
            "--name",
            "worker",
            "--repo-path",
            repo.to_str().expect("utf-8 path"),
            "--on-existing",
            mode,
            "--json",
        ])
    };

    // fail: exit 1, and the error names the session in the way — an integrator
    // acts on the id, not on the word "exists".
    let refused = create("fail");
    assert_eq!(
        refused.status.code(),
        Some(1),
        "a duplicate name must exit non-zero: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let doc = String::from_utf8_lossy(&refused.stdout);
    assert!(
        doc.contains(&existing),
        "the refusal names the existing id: {doc}"
    );

    // adopt: exit 0, the existing session, and `created: false` so a caller
    // reads one shape whether it made the session or found it.
    let adopted = create("adopt");
    assert_eq!(adopted.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&adopted.stdout).expect("one JSON document");
    assert_eq!(value["id"].as_str(), Some(existing.as_str()));
    assert_eq!(value["created"].as_bool(), Some(false));

    // The row is still there: adopting is not a mutation.
    let listed = env.run(&["session", "list", "--json"]);
    let rows: Value = serde_json::from_slice(&listed.stdout).expect("JSON");
    assert_eq!(rows.as_array().map(Vec::len), Some(1));
}

/// `--on-existing adopt --reports-as <agent>` must apply the declaration to
/// the row it hands back, not silently drop it: `resolve_existing` used to
/// return the adopted session before the create path's `reports_as` handling
/// ever ran, so a driver adopting an existing `--command` session and
/// declaring what actually runs in it got exit 0 with the declaration
/// discarded.
#[test]
fn adopt_applies_reports_as_to_the_session_it_hands_back() {
    let env = Env::new();
    let repo = env.path("home");
    let id = seed_command_session(&env, "worker");

    let out = env.run(&[
        "session",
        "create",
        "--name",
        "worker",
        "--repo-path",
        repo.to_str().expect("utf-8 path"),
        "--on-existing",
        "adopt",
        "--reports-as",
        "claude",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(doc["id"], Value::String(id.clone()));
    assert_eq!(doc["created"], Value::Bool(false));
    assert_eq!(doc["reports_as"], Value::String("claude".into()), "{doc}");

    let db = open_db(&env);
    let declared = db
        .load_reports_as()
        .expect("query")
        .get(&id.parse().expect("seeded id"))
        .cloned();
    assert_eq!(declared, Some("claude".into()));
}

/// A name matching more than one session is refused for `adopt` and `replace`,
/// because either would be a guess about which session was meant.
///
/// This is the same rule the reference resolver follows, and it has to hold
/// here too: thurbox does not enforce uniqueness by default, so a database
/// with two same-named rows is a state `create` can legitimately meet.
#[test]
fn an_ambiguous_name_is_never_adopted_or_replaced() {
    let env = Env::new();
    let repo = env.path("home");
    seed_session(&env, "twin", "claude");
    seed_session(&env, "twin", "codex");

    for mode in ["adopt", "replace"] {
        let out = env.run(&[
            "session",
            "create",
            "--name",
            "twin",
            "--repo-path",
            repo.to_str().expect("utf-8 path"),
            "--on-existing",
            mode,
            "--json",
        ]);
        assert_eq!(
            out.status.code(),
            Some(3),
            "--on-existing {mode} must refuse an ambiguous name with the ambiguity code"
        );
        let doc = String::from_utf8_lossy(&out.stdout);
        assert!(doc.contains('2'), "the refusal counts the matches: {doc}");
    }
}
