//! What survives a restart — of one session, and of thurbox itself.
//!
//! Both were divergences from v1 that only appear when something you were
//! relying on is missing, which is why neither showed up in an audit of what v2
//! *draws*:
//!
//!   * Restarting a session killed its window, spawned a new one, and left the
//!     interface attached to the pane it had just killed — a frozen last frame
//!     that takes no keys. v1 has no equivalent because `Session::restart`
//!     rebinds the live object in place.
//!   * Restarting *thurbox* after the tmux server had gone left every session
//!     unattached forever. v1 relaunches the agent when restore finds no
//!     matching window (`respawn_stale_session`), which is how a session
//!     survives a reboot.

use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::terminal::Terminals;

fn row(id: &str, name: &str, backend: &str, pane: Option<&str>) -> SessionRow {
    SessionRow {
        id: id.into(),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: None,
        repo: None,
        repos: Vec::new(),
        branch: None,
        base_branch: None,
        backend: backend.into(),
        backend_id: pane.map(str::to_string),
        remote_host: None,
        agent_session_id: Some("sid".into()),
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

/// The shell a session had open, as the row records it.
fn with_shell(mut row: SessionRow, pane: &str) -> SessionRow {
    row.shell_backend_id = Some(pane.into());
    row
}

fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
    Snapshot {
        sessions: rows,
        ..Snapshot::default()
    }
}

#[test]
fn nothing_is_relaunched_before_the_windows_have_been_looked_at() {
    // The distinction the whole respawn rests on: "we have not looked yet" and
    // "we looked and it is gone" are the same silence. Relaunching on the first
    // would start a second agent beside a perfectly good one.
    let terminals = Terminals::new();
    let rows = snapshot(vec![row("aaa", "demo", "local-tmux", None)]);
    assert!(
        terminals.missing_agents(&rows).is_empty(),
        "a backend nobody has surveyed cannot be said to be missing anything"
    );
}

#[test]
fn a_session_that_names_a_pane_is_not_treated_as_missing_its_agent() {
    // It is failing to attach to a pane it has, which is a different problem
    // with a different fix — relaunching would abandon a live agent.
    let terminals = Terminals::new();
    let rows = snapshot(vec![row("aaa", "demo", "local-tmux", Some("%7"))]);
    assert!(terminals.missing_agents(&rows).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_whose_window_is_gone_is_reported_as_missing_its_agent() {
    // The reboot case, in miniature: a real tmux server with no window for this
    // session. Skipped where tmux is not installed, like the other backend tests.
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(
        thurbox::agent::tmux::SOCKET_OVERRIDE_ENV,
        "thurbox-life-test",
    );

    let socket = ["-L", "thurbox-life-test"];
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["new-session", "-d", "-s", "thurbox-dev", "-n", "bash", "sh"])
        .output();

    let mut terminals = Terminals::new();
    let rows = snapshot(vec![row("aaa", "demo", "local-tmux", None)]);

    // Sync until the survey has happened — discovery is a worker, so the answer
    // arrives a moment after the question.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut missing = Vec::new();
    while std::time::Instant::now() < deadline {
        terminals.sync(&rows, 24, 80);
        missing = terminals.missing_agents(&rows);
        if !missing.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .arg("kill-server")
        .output();

    assert_eq!(
        missing,
        vec!["aaa".to_string()],
        "a surveyed backend with no window for this session means the agent is gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_whose_window_exists_is_not_relaunched() {
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, "thurbox-life-ok");

    let socket = ["-L", "thurbox-life-ok"];
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["new-session", "-d", "-s", "thurbox-dev", "-n", "bash", "sh"])
        .output();
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args([
            "new-window",
            "-t",
            "thurbox-dev",
            "-n",
            "tb-demo",
            "sh -c 'while :; do sleep 1; done'",
        ])
        .output();

    let mut terminals = Terminals::new();
    let rows = snapshot(vec![row("aaa", "demo", "local-tmux", None)]);
    for _ in 0..40 {
        terminals.sync(&rows, 24, 80);
        std::thread::sleep(std::time::Duration::from_millis(50));
        if terminals.is_attached("aaa") {
            break;
        }
    }
    let missing = terminals.missing_agents(&rows);
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .arg("kill-server")
        .output();

    assert!(
        missing.is_empty(),
        "its window is right there — relaunching would start a second agent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_created_after_the_last_survey_is_not_relaunched() {
    // The creation race. `spawn_session_headless` makes the tmux window first and
    // persists the row with an empty pane id after, so the row becomes visible
    // between surveys. Judged against the *older* window listing it is simply
    // absent — and "absent from a listing taken before you existed" was read as
    // "your agent is gone", so the interface killed the window the spawn had just
    // created and started a second agent in its place.
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(
        thurbox::agent::tmux::SOCKET_OVERRIDE_ENV,
        "thurbox-life-new",
    );

    let socket = ["-L", "thurbox-life-new"];
    let idle = "sh -c 'while :; do sleep 1; done'";
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["new-session", "-d", "-s", "thurbox-dev", "-n", "bash", "sh"])
        .output();
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["new-window", "-t", "thurbox-dev", "-n", "tb-alpha", idle])
        .output();

    // First, get a survey on the board — this is the "run that started with a
    // session" precondition, and it is what made the bug fire on every later
    // creation rather than the first.
    let mut terminals = Terminals::new();
    let alpha = snapshot(vec![row("aaa", "alpha", "local-tmux", None)]);
    for _ in 0..40 {
        terminals.sync(&alpha, 24, 80);
        if terminals.is_attached("aaa") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Now a second session appears, window first, exactly as spawn orders it.
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["new-window", "-t", "thurbox-dev", "-n", "tb-beta", idle])
        .output();
    let both = snapshot(vec![
        row("aaa", "alpha", "local-tmux", None),
        row("bbb", "beta", "local-tmux", None),
    ]);

    // Every sync from the moment it appears until it resolves, it must never once
    // be called missing: a single frame's worth is enough to kill it.
    let mut ever_missing: Vec<String> = Vec::new();
    for _ in 0..40 {
        terminals.sync(&both, 24, 80);
        ever_missing.extend(terminals.missing_agents(&both));
        if terminals.is_attached("bbb") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let attached = terminals.is_attached("bbb");

    // The other half of the contract: a row whose window genuinely is not there
    // must still be reported, once a survey has actually covered it.
    let ghost = snapshot(vec![
        row("aaa", "alpha", "local-tmux", None),
        row("ccc", "ghost", "local-tmux", None),
    ]);
    let mut ghost_missing = Vec::new();
    for _ in 0..40 {
        terminals.sync(&ghost, 24, 80);
        ghost_missing = terminals.missing_agents(&ghost);
        if !ghost_missing.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let _ = std::process::Command::new("tmux")
        .args(socket)
        .arg("kill-server")
        .output();

    assert!(
        ever_missing.is_empty(),
        "a session whose window was created moments ago is not missing its agent: {ever_missing:?}"
    );
    assert!(attached, "beta should have resolved to its window");
    assert_eq!(
        ghost_missing,
        vec!["ccc".to_string()],
        "a row with no window must still be relaunched once a survey has covered it"
    );
}

#[test]
fn nothing_is_producing_output_before_anything_is_attached() {
    // The loop compares this every iteration to decide whether an agent printed.
    // It has to be cheap and it has to be stable while nothing is happening, or
    // the screen repaints forever.
    let terminals = Terminals::new();
    assert_eq!(terminals.output_generation(), 0);
    assert_eq!(
        terminals.output_generation(),
        terminals.output_generation(),
        "an idle interface must not look like it is changing"
    );
}

#[test]
fn a_session_with_no_shell_recorded_is_not_asked_about() {
    // The re-adoption runs every iteration, so the common case — no shell — must
    // cost nothing and must not invent one.
    let terminals = Terminals::new();
    assert_eq!(terminals.shell_pane_id("aaa"), None);
    assert!(!terminals.has_shell("aaa"));
}

#[test]
fn a_row_carries_the_shell_it_had_open() {
    // The fact the interface forgot on every restart: the shell's window
    // outlives the process, but that this session had one lived only in a
    // `Session` object that does not.
    let plain = row("aaa", "demo", "local-tmux", None);
    assert_eq!(plain.shell_backend_id, None);
    let remembered = with_shell(row("aaa", "demo", "local-tmux", None), "%9");
    assert_eq!(remembered.shell_backend_id.as_deref(), Some("%9"));
}

#[test]
fn an_outcome_names_what_it_happened_to() {
    // `restart` alone leaves the reader working out which of six sessions it
    // was, and a bare error reads like a bug in thurbox rather than something
    // about their session.
    let failed =
        thurbox::kernel::messages::failed("restart", Some("fix-osc52"), "no window on that host");
    assert!(failed.contains("fix-osc52"), "{failed}");
    assert!(failed.contains("restart"), "{failed}");
    assert!(failed.contains("no window"), "{failed}");

    assert_eq!(
        thurbox::kernel::messages::done("sync", Some("fix-osc52")).as_deref(),
        Some("synced fix-osc52")
    );
}

#[test]
fn what_is_already_on_screen_is_not_announced() {
    // Announcing a reorder or a theme change trains the reader to ignore the
    // band, which is what makes the messages that matter unreadable.
    for quiet in ["reorder", "order", "theme", "setting", "focus", "reap"] {
        assert_eq!(
            thurbox::kernel::messages::done(quiet, Some("x")),
            None,
            "{quiet} is visible where it happened"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn letting_go_of_a_pane_makes_the_next_sync_attach_afresh() {
    // The freeze, and why detecting it does not work. `has_exited` is set when a
    // session's output STREAM ends, and tmux control mode carries every pane on a
    // backend down one connection — killing a pane leaves that stream open, so a
    // restarted session looked alive while showing a pane that no longer existed.
    // The restart has to *say* the pane is gone.
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| !out.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(
        thurbox::agent::tmux::SOCKET_OVERRIDE_ENV,
        "thurbox-forget-test",
    );
    let socket = ["-L", "thurbox-forget-test"];
    let window = |name: &str| {
        let _ = std::process::Command::new("tmux")
            .args(socket)
            .args([
                "new-window",
                "-t",
                "thurbox-dev",
                "-n",
                name,
                "sh -c 'while :; do sleep 1; done'",
            ])
            .output();
    };

    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["new-session", "-d", "-s", "thurbox-dev", "-n", "bash", "sh"])
        .output();
    window("tb-demo");

    let mut terminals = Terminals::new();
    let rows = snapshot(vec![row("aaa", "demo", "local-tmux", None)]);
    for _ in 0..60 {
        terminals.sync(&rows, 24, 80);
        if terminals.is_attached("aaa") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(terminals.is_attached("aaa"), "did not attach to begin with");

    // What a restart does: the window goes, a new one takes its name. The
    // interface is still holding the old pane, and nothing about the stream says
    // otherwise — which is exactly why it froze.
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .args(["kill-window", "-t", "thurbox-dev:tb-demo"])
        .output();
    window("tb-demo");
    terminals.sync(&rows, 24, 80);
    assert!(
        terminals.is_attached("aaa"),
        "still holding the dead pane, which is the bug this closes"
    );

    // Told, it lets go and re-attaches to the pane that is actually there.
    terminals.forget("aaa");
    assert!(!terminals.is_attached("aaa"));
    let mut back = false;
    for _ in 0..60 {
        terminals.sync(&rows, 24, 80);
        if terminals.is_attached("aaa") {
            back = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = std::process::Command::new("tmux")
        .args(socket)
        .arg("kill-server")
        .output();
    assert!(
        back,
        "letting go must be followed by attaching to the new pane"
    );
}
