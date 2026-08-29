//! What `thurbox-cli session get`/`list` say about a session's agent state.
//!
//! `hook_state` is self-reported and latched: once an agent writes `working` it
//! stays `working` until that agent writes something else, whether or not it is
//! still there. These tests drive the command an integrator actually calls and
//! assert what comes back — the age of the report, what the session's agent is
//! *able* to report, and what the pane's foreground process says about it.
//!
//! The pane half runs against a real tmux server on a private socket, for the
//! same reason `tests/cli_session_capture.rs` does: the thing under test is
//! what a real process tree looks like through `ps`, and a stub would assert
//! our own beliefs about that rather than the behaviour. Those tests skip where
//! tmux is not installed; the rest need no pane at all.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use thurbox::cli::sessions::{run, Action};
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// A socket of this test's own, so it can never see — or kill — a real session.
const SOCKET: &str = "thurbox-hookstate-test";

/// The tmux session name the local backend groups its windows under. Mirrors
/// `agent::tmux::TMUX_SESSION`, which is private — and is `thurbox-dev` here,
/// because a test build carries the same `dev_build` marker a dev binary does.
const SESSION: &str = "thurbox-dev";

/// An `agents.toml` in the shape the externally-driven integrations use: the
/// session's own "agent" is a bare interactive shell, because the driver owns
/// the real agent launch and starts it inside the pane itself.
const AGENTS_TOML: &str = r#"
config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"

[[agents]]
name = "codex"
command = "codex"

[[agents]]
name = "shell"
command = "bash"
args = ["-i"]
"#;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Whether this machine's `ps` speaks the accounting the foreground resolution
/// asks it for. A cut-down `ps` (busybox) knows none of these keywords, and the
/// pane assertions are then legitimately unanswerable.
fn have_ps() -> bool {
    Command::new("ps")
        .args([
            "-o",
            "pid=,pgid=,tpgid=,args=",
            "-p",
            &std::process::id().to_string(),
        ])
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-L")
        .arg(SOCKET)
        .args(args)
        .output()
        .expect("run tmux")
}

/// Point every thurbox path at a scratch dir and write the agent registry the
/// test wants there, so nothing reads or seeds the real `~/.config`.
fn isolated_config(dir: &Path) -> thurbox::paths::TestPathGuard {
    let guard = thurbox::paths::TestPathGuard::new(dir);
    let agents = thurbox::agent::agent_config::agents_config_path().expect("agents path");
    std::fs::create_dir_all(agents.parent().expect("config dir")).expect("mkdir");
    std::fs::write(&agents, AGENTS_TOML).expect("write agents.toml");
    guard
}

fn session_row(name: &str, agent: &str, backend_type: &str) -> SharedSession {
    SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: agent.into(),
        // Empty, so the pane is resolved by window name.
        backend_id: String::new(),
        backend_type: backend_type.into(),
        agent_session_id: None,
        cwd: None,
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    }
}

fn get(db: &Database, id: SessionId, verify: bool) -> thurbox::cli::output::CommandOutput {
    run(
        Action::Get {
            uuid: id.to_string(),
            no_verify: !verify,
        },
        db,
    )
    .expect("session get")
}

/// Poll `session get --verify` until the pane's process tree has settled, then
/// return the answer. tmux starts the window asynchronously, so an immediate
/// read can legitimately find nothing running yet.
fn get_when_pane_settles(
    db: &Database,
    id: SessionId,
    want: &str,
) -> thurbox::cli::output::CommandOutput {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let out = get(db, id, true);
        if out["hook_corroboration"] == Value::String(want.into())
            || std::time::Instant::now() >= deadline
        {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_reported_state_carries_its_age_and_its_agents_coverage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    let row = session_row("aged", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");
    db.set_hook_state(row.id, "working").expect("signal");

    let out = get(&db, row.id, false);

    // The word itself is untouched — a consumer that only ever read this keeps
    // reading exactly what it always did.
    assert_eq!(out["hook_state"], Value::String("working".into()));
    // …and now carries the two facts that make it judgeable.
    assert!(
        out["hook_state_at"].as_i64().is_some_and(|at| at > 0),
        "the report must carry when it was made: {out}"
    );
    assert!(
        out["hook_state_age_secs"].as_u64().is_some(),
        "the report must carry how old it is: {out}"
    );
    assert_eq!(out["hook_reported"], Value::Bool(true));
    assert_eq!(out["state"], Value::String("working".into()));
    assert_eq!(out["state_source"], Value::String("hook".into()));

    // claude can report every state, so its silence about one is informative.
    assert_eq!(out["hook_coverage"], Value::String("full".into()));
    let reportable: Vec<&str> = out["hook_states_reportable"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| v.as_str().expect("a state"))
        .collect();
    for state in ["working", "blocked", "done", "idle"] {
        assert!(reportable.contains(&state), "claude reports {state}: {out}");
    }
    // And its `blocked` is a text match on a notification body, which a
    // consumer has to know before reading anything into its absence.
    assert_eq!(out["hook_blocked_is_heuristic"], Value::Bool(true));
    assert_eq!(out["hook_delivery"], Value::String("args".into()));
}

#[test]
fn an_uninstrumented_session_is_not_reported_as_idle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    // The externally-driven shape: the session's agent is a bare shell, so the
    // hooks extension wires nothing and no hook can ever fire.
    let row = session_row("driver-owned", "shell", "local-tmux");
    db.upsert_session(&row).expect("persist");

    let out = get(&db, row.id, false);

    assert_eq!(out["hook_state"], Value::Null);
    assert_eq!(out["hook_reported"], Value::Bool(false));
    // The distinction the whole assessment exists for: this is *not* idle.
    assert_eq!(out["hook_coverage"], Value::String("none".into()));
    assert_eq!(out["hook_states_reportable"], serde_json::json!([]));
    assert_eq!(out["state"], Value::Null);
    assert_eq!(out["state_source"], Value::Null);
    assert!(
        out.human.contains("uncovered"),
        "the human rendering must say so too: {}",
        out.human
    );

    // An agent that can report only *some* states says so rather than passing
    // for a fully-instrumented one that happens to be quiet.
    let partial = session_row("partial", "aider", "local-tmux");
    db.upsert_session(&partial).expect("persist");
    let out = get(&db, partial.id, false);
    assert_eq!(out["hook_coverage"], Value::String("partial".into()));
    assert_eq!(
        out["hook_states_reportable"],
        serde_json::json!(["blocked"])
    );
}

#[test]
fn a_custom_agent_can_claim_a_hook_family() {
    let dir = tempfile::tempdir().expect("tempdir");
    let guard = thurbox::paths::TestPathGuard::new(dir.path());
    let agents = thurbox::agent::agent_config::agents_config_path().expect("agents path");
    std::fs::create_dir_all(agents.parent().expect("config dir")).expect("mkdir");
    std::fs::write(
        &agents,
        "config_version = 1\ndefault = \"fleet\"\n\n[[agents]]\nname = \"fleet\"\n\
         command = \"fleet\"\nhook_schema = \"claude\"\n",
    )
    .expect("write agents.toml");

    let db = Database::open_in_memory().expect("db");
    let row = session_row("rebrand", "fleet", "local-tmux");
    db.upsert_session(&row).expect("persist");

    let out = get(&db, row.id, false);
    assert_eq!(out["hook_coverage"], Value::String("full".into()));
    assert_eq!(
        out["hook_coverage_source"],
        Value::String("hook_schema".into()),
        "the user asserted this family; the report must say so: {out}"
    );
    drop(guard);
}

#[test]
fn a_remote_sessions_pane_is_reported_unreadable_rather_than_guessed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    let row = session_row("far-away", "claude", "ssh:devbox");
    db.upsert_session(&row).expect("persist");
    db.set_hook_state(row.id, "working").expect("signal");

    // A remote pane lives on its host's own multiplexer. Verifying is asked
    // for here and must come back honestly unanswerable, not half-answered.
    let out = get(&db, row.id, true);
    assert_eq!(
        out["hook_corroboration"],
        Value::String("unavailable".into())
    );
    assert_eq!(out["hook_state_contradicted"], Value::Null);
    assert_eq!(out["foreground_process"], Value::Null);
    // The reported state is untouched: unreadable is not disbelieved.
    assert_eq!(out["hook_state"], Value::String("working".into()));
    assert_eq!(out["state"], Value::String("working".into()));
}

#[test]
fn an_unverified_read_says_nothing_about_the_pane() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    let row = session_row("unchecked", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");

    // Not-checked is a third answer, distinct from checked-and-found-nothing:
    // `session list` pays for no probe unless asked, and must not imply one.
    let out = get(&db, row.id, false);
    assert_eq!(out["hook_corroboration"], Value::Null);
    assert_eq!(out["hook_state_contradicted"], Value::Null);
}

#[test]
fn a_working_state_over_a_bare_shell_is_reported_as_contradicted() {
    if !have_tmux() || !have_ps() {
        eprintln!("skipping: needs tmux and a ps that knows tpgid");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);
    // The agent is gone and a bare shell holds the pane — the exact shape a
    // crashed or interrupted agent leaves behind.
    tmux(&[
        "new-window",
        "-t",
        SESSION,
        "-n",
        "tb-lost-agent",
        "while :; do sleep 1; done",
    ]);

    let db = Database::open_in_memory().expect("db");
    let row = session_row("lost-agent", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");
    db.set_hook_state(row.id, "working").expect("signal");

    let out = get_when_pane_settles(&db, row.id, "shell");
    tmux(&["kill-server"]);

    assert_eq!(
        out["hook_corroboration"],
        Value::String("shell".into()),
        "a shell holds this pane: {out}"
    );
    assert_eq!(
        out["hook_state_contradicted"],
        Value::Bool(true),
        "a `working` agent that is not there is the whole problem: {out}"
    );
    // Reported, never applied: the agent's own word is left exactly as it
    // wrote it, so the contradiction is a second fact rather than a rewrite.
    assert_eq!(out["hook_state"], Value::String("working".into()));
    assert_eq!(out["state"], Value::String("working".into()));
    assert_eq!(out["state_source"], Value::String("hook".into()));
}

#[test]
fn an_agent_thurbox_did_not_launch_is_still_reported_as_running() {
    if !have_tmux() || !have_ps() {
        eprintln!("skipping: needs tmux and a ps that knows tpgid");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());

    // A stand-in for the real binary: what matters is that the pane's
    // foreground process is named after an agent the registry knows.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    let fake = bin.join("codex");
    std::fs::write(&fake, "#!/bin/sh\nwhile :; do sleep 1; done\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);
    tmux(&[
        "new-window",
        "-t",
        SESSION,
        "-n",
        "tb-driver-owned",
        &fake.to_string_lossy(),
    ]);

    let db = Database::open_in_memory().expect("db");
    // thurbox launched a bare shell for a driver that owns the agent launch,
    // so no hook was ever wired and nothing has ever signalled.
    let row = session_row("driver-owned", "shell", "local-tmux");
    db.upsert_session(&row).expect("persist");

    let out = get_when_pane_settles(&db, row.id, "foreign-agent");
    tmux(&["kill-server"]);

    assert_eq!(
        out["hook_corroboration"],
        Value::String("foreign-agent".into()),
        "an agent thurbox did not launch is still an agent: {out}"
    );
    assert_eq!(out["hook_state"], Value::Null, "nothing ever signalled");
    // Which is the point: the session is no longer indistinguishable from an
    // empty one, and the coarser provenance says how far to trust it.
    assert_eq!(out["state"], Value::String("running".into()));
    assert_eq!(out["state_source"], Value::String("process".into()));
    assert_eq!(out["hook_state_contradicted"], Value::Bool(false));
    assert!(out["foreground_command"]
        .as_str()
        .is_some_and(|c| c.contains("codex")));
}
