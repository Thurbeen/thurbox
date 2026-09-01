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

/// Pin `PATH` for the duration of a test, restoring what was there on drop.
///
/// `session doctor` looks for `thurbox-cli` the way a hook command does — by
/// bare name on `PATH` — and reports `FAIL` when it is absent. That is the
/// check under test in one assertion and pure noise in every other, so the
/// operator's install layout must not decide it: a machine with the binary in
/// `~/.local/bin` and a CI runner without one would otherwise disagree about
/// every verdict below.
struct PathGuard(Option<std::ffi::OsString>);

impl PathGuard {
    /// `PATH` holding exactly `dir`, which is created and — when `cli` — given
    /// a file named the way a hook command spells the binary.
    fn only(dir: &Path, cli: bool) -> Self {
        let previous = std::env::var_os("PATH");
        std::fs::create_dir_all(dir).expect("mkdir");
        let named = dir.join(format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX));
        if cli {
            std::fs::write(&named, "#!/bin/sh\nexit 0\n").expect("write");
        } else {
            let _ = std::fs::remove_file(&named);
        }
        std::env::set_var("PATH", dir);
        Self(previous)
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(previous) => std::env::set_var("PATH", previous),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// One session's doctor report.
fn doctor(db: &Database, id: SessionId) -> thurbox::cli::output::CommandOutput {
    run(
        Action::Doctor {
            uuid: Some(id.to_string()),
        },
        db,
    )
    .expect("doctor runs")
}

/// One named check out of a report.
fn check(out: &thurbox::cli::output::CommandOutput, key: &str) -> Value {
    out.json.as_array().expect("reports")[0]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["check"] == Value::String(key.into()))
        .unwrap_or_else(|| panic!("{key} is checked: {out}"))
        .clone()
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
    // `state` is a word rather than a null: a bare null cannot tell "no hooks
    // for this agent" from "wired and quiet". `state_source` staying null is
    // what marks it as nobody's report.
    assert_eq!(out["state"], Value::String("uncovered".into()));
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
    // Wired but quiet is the other silence, and it gets its own word.
    assert_eq!(out["state"], Value::String("unreported".into()));
    assert_eq!(out["state_source"], Value::Null);
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
fn a_session_with_no_pane_of_its_own_is_told_apart_from_a_strangers() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());

    // A server with a window that is *not* this session's. `display-message`
    // against a target it cannot resolve answers for the client's current pane
    // and exits 0, so an unguarded probe reports this stranger's process as the
    // session's foreground — a confident, entirely wrong answer.
    tmux(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-n",
        "someone-else",
        "sh",
    ]);

    let db = Database::open_in_memory().expect("db");
    let row = session_row("never-launched", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");
    db.set_hook_state(row.id, "working").expect("signal");

    let out = get(&db, row.id, true);
    tmux(&["kill-server"]);

    assert_eq!(
        out["hook_corroboration"],
        Value::String("unknown".into()),
        "nothing of this session is running, and nothing may be invented: {out}"
    );
    assert_eq!(out["foreground_process"], Value::Null);
    assert_eq!(
        out["hook_state_contradicted"],
        Value::Bool(false),
        "an unresolvable pane disproves nothing: {out}"
    );
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

#[test]
fn doctor_names_the_wiring_that_is_missing_and_exits_non_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    // Every verdict below is about *hook* wiring, so the one check that reads
    // the machine — `thurbox-cli` on PATH — is pinned present rather than left
    // to whether the operator happens to have installed it.
    let _path = PathGuard::only(&dir.path().join("path"), true);
    let db = Database::open_in_memory().expect("db");
    let row = session_row("unwired", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");

    let out = doctor(&db, row.id);

    // Nothing was ever installed into this scratch config, so claude's hooks
    // cannot fire — and the whole point is that this is *sayable* rather than
    // indistinguishable from an agent that has not signalled yet.
    let report = out.json.as_array().expect("one report per session")[0].clone();
    assert_eq!(report["verdict"], Value::String("fail".into()), "{report}");
    let payload = check(&out, "payload");
    assert_eq!(payload["level"], Value::String("fail".into()));
    assert!(
        payload["detail"]
            .as_str()
            .is_some_and(|d| d.contains("claude.json")),
        "the report must name the file the agent reads: {payload}"
    );
    // Scriptable: a broken session is an exit code, not a sentence to grep.
    assert!(out.failure.is_some(), "doctor must exit non-zero: {out}");

    // …and an agent that reports nothing at all is a failure with a route out
    // of it, not a shrug.
    let driver = session_row("driver-owned", "shell", "local-tmux");
    db.upsert_session(&driver).expect("persist");
    let out = doctor(&db, driver.id);
    let coverage = check(&out, "coverage");
    assert_eq!(coverage["level"], Value::String("fail".into()));
    assert!(
        coverage["detail"]
            .as_str()
            .is_some_and(|d| d.contains("session signal") && d.contains("THURBOX_SESSION")),
        "an integrator has no reason to know the signal route exists: {coverage}"
    );

    // …but once that driver takes the route, state is demonstrably arriving,
    // and a verdict of `fail` — with the non-zero exit behind it — would be
    // false for exactly the shape this feature exists to serve.
    db.set_hook_state(driver.id, "working").expect("signal");
    let out = doctor(&db, driver.id);
    let report = out.json.as_array().expect("reports")[0].clone();
    assert_eq!(report["verdict"], Value::String("warn".into()), "{report}");
    assert!(
        out.failure.is_none(),
        "a session that is reporting must not exit non-zero: {out}"
    );
    assert_eq!(check(&out, "cli")["level"], Value::String("ok".into()));
}

#[test]
fn doctor_fails_a_session_whose_hook_command_cannot_find_the_binary_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    // A session that is otherwise as healthy as this scratch config gets: its
    // driver signals for itself, so coverage is a warning rather than a
    // failure and the `cli` check is the only thing left that can fail.
    let row = session_row("driver-owned", "shell", "local-tmux");
    db.upsert_session(&row).expect("persist");
    db.set_hook_state(row.id, "working").expect("signal");

    let empty = dir.path().join("empty-path");
    {
        let _path = PathGuard::only(&empty, false);
        let out = doctor(&db, row.id);
        let cli = check(&out, "cli");
        assert_eq!(cli["level"], Value::String("fail".into()), "{cli}");
        assert!(
            cli["detail"].as_str().is_some_and(|d| d.contains("PATH")),
            "the report must say where it looked: {cli}"
        );
        assert!(
            out.failure.is_some(),
            "a hook that cannot find its binary signals nothing: {out}"
        );
    }

    // Same session, same database, same everything but the one fact under
    // test — so the verdict is the wiring's and not the machine's.
    let _path = PathGuard::only(&dir.path().join("with-cli"), true);
    let out = doctor(&db, row.id);
    assert_eq!(check(&out, "cli")["level"], Value::String("ok".into()));
    assert!(out.failure.is_none(), "{out}");
}

/// A session parked by `session stop` must be tellable from a running one by
/// the two verbs a driver polls.
///
/// It used to be readable only through `watch`, which reports the flag — so the
/// alternative was probing the pane and inferring, or paying a one-second
/// `watch --initial` per liveness check. `get` and `list` answered *identically*
/// for a parked and a running session, down to a `backend_id` naming a window
/// that no longer existed.
#[test]
fn a_parked_session_says_so_on_get_and_on_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    let row = session_row("parked", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");
    db.set_hook_state(row.id, "working").expect("signal");

    // Running: the agent's own last word, and not stopped.
    let before = get(&db, row.id, false);
    assert_eq!(before["state"], Value::String("working".into()));
    assert_eq!(before["stopped"], Value::Bool(false));

    run(
        Action::Stop {
            session: row.id.to_string(),
        },
        &db,
    )
    .expect("session stop");

    let after = get(&db, row.id, false);
    assert_eq!(after["stopped"], Value::Bool(true), "{after}");
    // Not `working`, and not `uncovered` either: it is parked, which is a fact
    // thurbox knows first-hand rather than one inferred from an agent's
    // silence. Both of the other answers describe a session that is running.
    assert_eq!(after["state"], Value::String("stopped".into()), "{after}");
    assert!(
        after["state_source"].is_null(),
        "nothing reported this; thurbox recorded it: {after}"
    );

    // And the same fact under the same key on the list, which is the verb a
    // driver actually polls.
    let listed = run(
        Action::List {
            parent: None,
            deleted: false,
            verify: false,
        },
        &db,
    )
    .expect("session list");
    let rows = listed.json.as_array().expect("rows");
    let found = rows
        .iter()
        .find(|r| r["id"] == Value::String(row.id.to_string()))
        .expect("a parked session stays in the list");
    assert_eq!(found["stopped"], Value::Bool(true), "{found}");
    assert_eq!(found["state"], Value::String("stopped".into()), "{found}");
}

/// The pane verbs refuse a parked session by name.
///
/// `session stop` killed the window on purpose, so reaching for it and
/// reporting what the multiplexer says about a window that is not there
/// describes a crash rather than the state the caller itself asked for.
#[test]
fn the_pane_verbs_refuse_a_parked_session_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let db = Database::open_in_memory().expect("db");
    let row = session_row("no-pane", "claude", "local-tmux");
    db.upsert_session(&row).expect("persist");
    run(
        Action::Stop {
            session: row.id.to_string(),
        },
        &db,
    )
    .expect("session stop");

    for action in [
        Action::Send {
            uuid: row.id.to_string(),
            text: "hello".into(),
            no_enter: false,
        },
        Action::Key {
            uuid: row.id.to_string(),
            key: "enter".into(),
        },
        Action::Capture {
            uuid: row.id.to_string(),
            lines: 10,
            ansi: false,
        },
    ] {
        let err = run(action, &db).expect_err("a parked session has no pane");
        assert!(err.contains("stopped"), "got {err}");
        assert!(
            err.contains("session start"),
            "the refusal names the fix: {err}"
        );
    }
}

/// `session doctor` on a parked session is a clean report, not a warning about
/// silence it was told to cause.
///
/// `aider` only ever reports `blocked` (`Coverage::Partial`, and no
/// `hook_file`), so its coverage and payload checks stay put across the stop —
/// the only thing that moves is what the doctor makes of a session it knows was
/// asked to go silent. Before `stop`, a session that has never signalled gets a
/// `warn` on `last-signal` — the honest "nothing has ever signalled" case. Once
/// `stop` parks it, the same absence of a signal is expected and reported
/// `ok`, and the pane check — which would otherwise warn that nothing could be
/// resolved — says plainly that there is no pane by design.
#[test]
fn a_parked_sessions_doctor_report_is_clean_not_a_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = isolated_config(dir.path());
    let _path = PathGuard::only(&dir.path().join("path"), true);
    let db = Database::open_in_memory().expect("db");
    let row = session_row("parked-doctor", "aider", "local-tmux");
    db.upsert_session(&row).expect("persist");

    let before = doctor(&db, row.id);
    assert_eq!(
        check(&before, "last-signal")["level"],
        Value::String("warn".into()),
        "{before}"
    );

    run(
        Action::Stop {
            session: row.id.to_string(),
        },
        &db,
    )
    .expect("session stop");

    let after = doctor(&db, row.id);
    let last_signal = check(&after, "last-signal");
    assert_eq!(
        last_signal["level"],
        Value::String("ok".into()),
        "{last_signal}"
    );

    let pane = check(&after, "pane");
    assert_eq!(pane["level"], Value::String("ok".into()), "{pane}");
    assert!(
        pane["detail"]
            .as_str()
            .is_some_and(|d| d.contains("stopped") && d.contains("session start")),
        "the report must say why there is no pane and how to get one back: {pane}"
    );

    assert!(
        after.failure.is_none(),
        "a parked session is not broken wiring: {after}"
    );
}
