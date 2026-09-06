//! What makes a session's dot say `working`, and what makes it stop.
//!
//! The regression these guard: the fallback that rescues a stuck `working`
//! state was keyed on the age of the hook row rather than on terminal output,
//! so every turn reported itself finished ten seconds in and started again at
//! the agent's next hook — a spinner that stopped early and restarted while the
//! agent was still visibly printing. Hooks only mark the edges of a turn; only
//! the terminal says whether one is still running.
//!
//! Driven through `SnapshotStore` rather than the pure fold, because the part
//! that can be wrong is that the pass runs every tick and reverses itself: a
//! session that goes quiet and then prints again has to be `working` once more
//! without waiting for a hook that is never coming.

use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// Longer than the fallback's bound, whatever it is set to.
const QUIET: u64 = 60_000;

/// Every session is printing right now.
fn printing(_id: &str) -> Option<u64> {
    Some(0)
}

/// Every session has a live pane that has said nothing for a long time.
fn silent(_id: &str) -> Option<u64> {
    Some(QUIET)
}

/// No session has a live pane at all.
fn detached(_id: &str) -> Option<u64> {
    None
}

fn store_with_working_session() -> (tempfile::TempDir, SnapshotStore, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join("thurbox.db");
    let row = SharedSession {
        id: SessionId::default(),
        name: "long-turn".into(),
        agent: "claude".into(),
        backend_id: "%3".into(),
        backend_type: "local-tmux".into(),
        agent_session_id: Some("sid".into()),
        cwd: Some(std::path::PathBuf::from("/srv/repo")),
        additional_dirs: Vec::new(),
        worktrees: Vec::new(),
        shell_backend_id: None,
        parent_session_id: None,
        display_order: None,
        tombstone: false,
        tombstone_at: None,
    };
    let database = Database::open(&path).expect("db");
    database.upsert_session(&row).expect("upsert");
    database.set_hook_state(row.id, "working").expect("signal");
    let mut store = SnapshotStore::with_database(Database::open(&path).expect("db"));
    store.refresh();
    (home, store, row.id.to_string())
}

fn status_of(store: &SnapshotStore, id: &str) -> String {
    store
        .current()
        .sessions
        .iter()
        .find(|row| row.id == id)
        .map(|row| row.status.as_str().to_string())
        .unwrap_or_else(|| "<missing>".to_string())
}

/// A hook fired long ago and never followed up, with the agent printing the
/// whole time. This is an ordinary turn that outlasts the fallback's bound.
#[test]
fn a_long_turn_keeps_working_while_its_agent_prints() {
    let (_home, mut store, id) = store_with_working_session();
    assert_eq!(status_of(&store, &id), "working");

    for _ in 0..3 {
        assert_eq!(store.apply_output_quiescence(printing), 0);
        assert_eq!(status_of(&store, &id), "working");
    }
}

/// The turn was interrupted, which fires no hook at all: the row still says
/// `working` and nothing is left to print.
#[test]
fn a_turn_that_went_quiet_is_reported_idle() {
    let (_home, mut store, id) = store_with_working_session();

    assert_eq!(store.apply_output_quiescence(silent), 1);
    assert_eq!(status_of(&store, &id), "idle");
    // Idempotent: the second pass has nothing left to change.
    assert_eq!(store.apply_output_quiescence(silent), 0);
}

/// A session absent from the map has no live pane, so nothing can be producing
/// output — where the old exited → `Idle` branch lands.
#[test]
fn a_working_session_with_no_live_pane_is_reported_idle() {
    let (_home, mut store, id) = store_with_working_session();
    assert_eq!(store.apply_output_quiescence(detached), 1);
    assert_eq!(status_of(&store, &id), "idle");
}

/// The half a one-way fallback gets wrong. An agent that goes quiet mid-turn
/// and then resumes fires no hook to say so, so the pass has to re-derive from
/// `hook_state` rather than adjust the status it wrote last time.
#[test]
fn a_session_that_prints_again_goes_back_to_working() {
    let (_home, mut store, id) = store_with_working_session();

    assert_eq!(store.apply_output_quiescence(silent), 1);
    assert_eq!(status_of(&store, &id), "idle");

    assert_eq!(store.apply_output_quiescence(printing), 1);
    assert_eq!(status_of(&store, &id), "working");
}

/// A session waiting on you produces no output while it waits, and saying so is
/// the whole point of the dot. Only `working` is time-gated.
#[test]
fn a_blocked_session_is_never_time_gated() {
    let (home, mut store, id) = store_with_working_session();
    let database = Database::open(&home.path().join("thurbox.db")).expect("db");
    let parsed: SessionId = id.parse().expect("id");
    database.set_hook_state(parsed, "blocked").expect("signal");
    store.refresh();
    assert_eq!(status_of(&store, &id), "blocked");

    assert_eq!(store.apply_output_quiescence(silent), 0);
    assert_eq!(status_of(&store, &id), "blocked");
}

// --- what actually holds the pane ------------------------------------------

/// A socket of this test's own, so it can never see — or kill — a real session.
const PROBE_SOCKET: &str = "thurbox-probe-test";

/// The tmux session name the local backend groups its windows under. Mirrors
/// `agent::tmux::TMUX_SESSION`, which is private — and is `thurbox-dev` here,
/// because a test build carries the same `dev_build` marker a dev binary does.
const TMUX_SESSION: &str = "thurbox-dev";

/// The externally-driven registry: the session's own "agent" is a bare shell,
/// because the driver owns the real agent launch and starts it in the pane.
const AGENTS_TOML: &str = r#"
config_version = 1
default = "claude"

[[agents]]
name = "claude"
command = "claude"

[[agents]]
name = "shell"
command = "bash"
args = ["-i"]
"#;

fn have_tmux() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Whether this machine's `ps` speaks the accounting the foreground resolution
/// asks it for. A cut-down `ps` (busybox) knows none of these keywords.
fn have_ps() -> bool {
    std::process::Command::new("ps")
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
    std::process::Command::new("tmux")
        .arg("-L")
        .arg(PROBE_SOCKET)
        .args(args)
        .output()
        .expect("run tmux")
}

/// An executable named after an agent, so the pane's process tree is what the
/// probe would really see.
fn fake_agent(bin: &std::path::Path, name: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(bin).expect("mkdir");
    let path = bin.join(name);
    std::fs::write(&path, "#!/bin/sh\nwhile :; do sleep 1; done\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    path
}

/// Turn the loop until the probe's answer has landed, or give up.
///
/// The verdict is computed on a worker thread and arrives through
/// `refresh_if_due`, which is also rate-limited — so this is the loop's own
/// cadence, not a poll of the pane.
fn settle(store: &mut SnapshotStore, id: &str, want: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        store.refresh_if_due();
        let state = status_of(store, id);
        if state == want || std::time::Instant::now() >= deadline {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// R1 + R3, end to end and through a real process tree: the interface's own
/// derivation, the pane probe that feeds it, and the name it publishes.
///
/// The captain's symptom lives exactly here — a driver asks thurbox for a bare
/// shell, starts an agent in the pane itself, and nothing is wired to report a
/// turn. The interface used to draw the green hollow `idle` dot for that, which
/// says the agent reported it is at rest. It now says an agent is running,
/// names it, and still claims nothing about the turn.
#[test]
fn an_agent_a_driver_started_is_seen_and_named_by_the_interface() {
    if !have_tmux() || !have_ps() {
        eprintln!("skipping: needs tmux and a ps that knows tpgid");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, PROBE_SOCKET);
    let guard = thurbox::paths::TestPathGuard::new(home.path());
    let agents = thurbox::agent::agent_config::agents_config_path().expect("agents path");
    std::fs::create_dir_all(agents.parent().expect("config dir")).expect("mkdir");
    std::fs::write(&agents, AGENTS_TOML).expect("write agents.toml");

    let row = SharedSession {
        id: SessionId::default(),
        name: "driver-owned".into(),
        // What the driver asked thurbox for. thurbox wires no hooks for it.
        agent: "shell".into(),
        // Empty, so the pane is resolved by window name.
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
    let path = home.path().join("thurbox.db");
    Database::open(&path)
        .expect("db")
        .upsert_session(&row)
        .expect("persist");

    // The driver's shape: thurbox opened a pane, the driver started an agent in
    // it, and nothing was ever wired to report a turn.
    let claude = fake_agent(&home.path().join("bin"), "claude");
    tmux(&["new-session", "-d", "-s", TMUX_SESSION, "-n", "bash", "sh"]);
    tmux(&[
        "new-window",
        "-t",
        TMUX_SESSION,
        "-n",
        "tb-driver-owned",
        &claude.to_string_lossy(),
    ]);

    let mut store = SnapshotStore::with_database(Database::open(&path).expect("db"));
    let id = row.id.to_string();

    // Something is running here — and that is the whole claim. `working` would
    // be one the observation cannot support, and `idle` is the one this fix
    // exists to stop: it says the agent reported that it is at rest.
    assert_eq!(settle(&mut store, &id, "running"), "running");
    assert_eq!(detected_of(&store, &id), Some("claude".to_string()));
    // Beside the name the row was created with, never instead of it.
    assert_eq!(agent_of(&store, &id), "shell");

    // Now the same pane holding nothing but a process whose argv *mentions* an
    // agent — the shape a driver's multi-kilobyte brief produces, and the false
    // positive that made a bare pane report an agent (F6). Reaching it from
    // `running` is what proves a fresh verdict was taken rather than a stale
    // one left standing.
    tmux(&[
        "kill-window",
        "-t",
        &format!("{TMUX_SESSION}:tb-driver-owned"),
    ]);
    tmux(&[
        "new-window",
        "-t",
        TMUX_SESSION,
        "-n",
        "tb-driver-owned",
        "perl",
        "-e",
        "sleep 300",
        "claude",
    ]);

    let inert = settle(&mut store, &id, "uncovered");
    tmux(&["kill-server"]);
    drop(guard);

    assert_eq!(
        inert, "uncovered",
        "a pane merely mentioning an agent must not report one"
    );
    assert_eq!(detected_of(&store, &id), None);
}

fn row_of<'a>(store: &'a SnapshotStore, id: &str) -> &'a thurbox::kernel::snapshot::SessionRow {
    store
        .current()
        .sessions
        .iter()
        .find(|row| row.id == id)
        .expect("the session is in the snapshot")
}

fn detected_of(store: &SnapshotStore, id: &str) -> Option<String> {
    row_of(store, id).detected_agent.clone()
}

fn agent_of(store: &SnapshotStore, id: &str) -> String {
    row_of(store, id).agent.clone()
}
