//! What `thurbox-cli session capture` reports about a live pane.
//!
//! Driven against a real tmux server on a private socket, because the thing
//! under test is what tmux actually answers: a pane with known contents, a
//! known cursor position, a known foreground process and a known working
//! directory, read back through the command an integrator calls. A test that
//! stubbed tmux would assert our own beliefs about `#{cursor_y}` rather than
//! tmux's behaviour, which is the half that has to be right.
//!
//! Skipped where tmux is not installed, like `tests/create_e2e.rs`.

use std::process::Command;

use serde_json::Value;
use thurbox::cli::sessions::{run, Action};
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// A socket of this test's own, so it can never see — or kill — a real session.
const SOCKET: &str = "thurbox-capture-test";

/// The tmux session name the local backend groups its windows under. Mirrors
/// `agent::tmux::TMUX_SESSION`, which is private — and is `thurbox-dev` here,
/// because a test build carries the same `dev_build` marker a dev binary does.
const SESSION: &str = "thurbox-dev";

/// Printed by the pane, and carried in the argv that printed it — so one string
/// proves both the capture and the foreground-process resolution.
const MARKER: &str = "thurbox-capture-probe";

/// Newlines the probe prints after its marker line. The pane starts empty and
/// nothing else writes to it, so the cursor lands on exactly this row.
const TRAILING_NEWLINES: u32 = 3;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Whether this machine's `ps` speaks the accounting `pane_state` asks it for.
/// A cut-down `ps` (busybox) knows none of these keywords, and the fields it
/// feeds are then legitimately absent — so the assertions that need it are
/// asked for only where it can answer.
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

fn session_row(name: &str, backend_type: &str) -> SharedSession {
    SharedSession {
        id: SessionId::default(),
        name: name.into(),
        agent: "shell".into(),
        // Empty, so the pane is resolved by window name — the shape a row
        // persisted before pane ids were recorded still has.
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

/// Capture until the probe's output has actually reached the screen — tmux
/// paints asynchronously, so an immediate read can legitimately be blank.
fn capture_when_ready(
    db: &Database,
    id: SessionId,
    ansi: bool,
) -> thurbox::cli::output::CommandOutput {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let out = run(
            Action::Capture {
                uuid: id.to_string(),
                lines: 50,
                ansi,
            },
            db,
        )
        .expect("capture should succeed for a live local pane");
        if out["output"].as_str().unwrap_or_default().contains(MARKER)
            || std::time::Instant::now() >= deadline
        {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn capture_reports_the_panes_cursor_foreground_process_and_live_cwd() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // Private socket directory as well as a private socket name: the sandbox
    // pattern, so nothing here can reach a real thurbox server.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);

    // The pane's directory is deliberately *not* the session's recorded `cwd`
    // (left `None` below): `foreground_cwd` has to come from the live pane, not
    // from the row, which is the whole reason the field exists.
    let workdir = tempfile::tempdir().expect("tempdir");
    let workdir_path = workdir.path().canonicalize().expect("canonicalize");

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);
    let script = format!(
        "printf '{MARKER}\\033[31mRED\\033[0m{}'; while :; do sleep 1; done",
        "\\n".repeat(TRAILING_NEWLINES as usize)
    );
    tmux(&[
        "new-window",
        "-t",
        SESSION,
        "-n",
        "tb-capture-probe",
        "-c",
        &workdir_path.to_string_lossy(),
        &script,
    ]);

    let db = Database::open_in_memory().expect("db");
    let row = session_row("capture-probe", "local-tmux");
    let id = row.id;
    db.upsert_session(&row).expect("persist");

    let plain = capture_when_ready(&db, id, false);
    let ansi = capture_when_ready(&db, id, true);
    tmux(&["kill-server"]);

    let text = plain["output"].as_str().expect("output is a string");
    assert!(
        text.contains(MARKER),
        "the pane's text should come back: {text:?}"
    );
    assert!(
        text.contains("RED") && !text.contains('\x1b'),
        "the default capture is plain text: {text:?}"
    );
    // The human rendering is still the pane text and nothing else — the new
    // fields are additive, so a caller that piped `--text` into a grep sees
    // exactly what it always did.
    assert_eq!(plain.human, text);

    // Styling survives only when it was asked for.
    let styled = ansi["output"].as_str().expect("output is a string");
    assert!(
        styled.contains('\x1b') && styled.contains("RED"),
        "--ansi should keep the escape sequences: {styled:?}"
    );
    assert_eq!(plain["ansi"], Value::Bool(false));
    assert_eq!(ansi["ansi"], Value::Bool(true));

    // The marker line plus its trailing newlines, on a pane nothing else wrote
    // to — so the row the cursor rests on is known exactly, and 0-based.
    assert_eq!(
        plain["cursor_row"].as_u64(),
        Some(u64::from(TRAILING_NEWLINES)),
        "cursor_row is 0-based and relative to the visible pane: {plain}"
    );
    assert_eq!(
        plain["cursor_col"].as_u64(),
        Some(0),
        "a newline leaves the cursor at column 0: {plain}"
    );

    // Where the pane is, which is not where the session says it was launched.
    assert_eq!(plain["cwd"], Value::Null, "the row carries no launch cwd");
    assert_eq!(
        plain["foreground_cwd"].as_str().map(std::path::Path::new),
        Some(workdir_path.as_path()),
        "foreground_cwd is the live pane's directory: {plain}"
    );

    // tmux always names the foreground command; only `ps` can produce its argv.
    assert!(
        plain["foreground_process"]
            .as_str()
            .is_some_and(|p| !p.is_empty()),
        "a live pane always has something in the foreground: {plain}"
    );
    if have_ps() {
        let command = plain["foreground_command"]
            .as_str()
            .unwrap_or_else(|| panic!("ps can answer here, so the argv must be reported: {plain}"));
        // The point of the field: a command *name* is the interpreter, and only
        // the argv says which program it is running.
        assert!(
            command.contains(MARKER),
            "foreground_command is the whole argv: {command:?}"
        );
    }
}

#[test]
fn capture_of_a_remote_session_goes_to_its_host_or_says_why_it_cannot() {
    let db = Database::open_in_memory().expect("db");
    let row = session_row("remote-probe", "ssh:devbox");
    db.upsert_session(&row).expect("persist");

    // A remote session's pane lives on its host's own tmux server. `capture`
    // used to refuse that outright, which made `--host` a shape thurbox could
    // create and then not drive; it now delegates to the host's own CLI.
    //
    // This fixture has no `hosts.toml` entry for `devbox`, which is the one
    // case where delegation is genuinely impossible — so a refusal is still
    // right, and it now names the actual obstacle (the missing host) rather
    // than an architectural limit that no longer exists. Delegation against a
    // real host is covered by the harnesses in `scripts/dev/e2e`.
    let err = run(
        Action::Capture {
            uuid: row.id.to_string(),
            lines: 50,
            ansi: false,
        },
        &db,
    )
    .expect_err("no hosts.toml entry means there is nowhere to delegate to");
    assert!(err.contains("ssh:devbox"), "got {err}");
    assert!(
        err.contains("hosts.toml"),
        "the refusal names what is missing: {err}"
    );
}

#[test]
fn capture_still_rejects_an_unusable_uuid() {
    // Unchanged from before the new fields: a malformed or unknown id is an
    // error, never an empty capture with null state.
    let db = Database::open_in_memory().expect("db");
    for uuid in ["not-a-uuid", "11111111-1111-1111-1111-111111111111"] {
        assert!(
            run(
                Action::Capture {
                    uuid: uuid.into(),
                    lines: 50,
                    ansi: false,
                },
                &db,
            )
            .is_err(),
            "{uuid} should not capture"
        );
    }
}

#[test]
fn capture_reports_pane_state_under_a_non_utf8_locale() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    // A client tmux does not believe speaks UTF-8 gets its output sanitized:
    // every control byte is rewritten to `_`, the separator `pane_state` joins
    // its fields with included. That is the ordinary environment of a systemd
    // unit, a cron job or a container with no locale set, so the state fields
    // must survive it rather than all coming back null.
    std::env::set_var("LC_ALL", "C");
    std::env::set_var("LANG", "C");
    std::env::remove_var("LC_CTYPE");

    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", home.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);

    let workdir = tempfile::tempdir().expect("tempdir");
    let workdir_path = workdir.path().canonicalize().expect("canonicalize");

    tmux(&["new-session", "-d", "-s", SESSION, "-n", "bash", "sh"]);
    let script = format!("printf '{MARKER}\\n'; while :; do sleep 1; done");
    tmux(&[
        "new-window",
        "-t",
        SESSION,
        "-n",
        "tb-locale-probe",
        "-c",
        &workdir_path.to_string_lossy(),
        &script,
    ]);

    let db = Database::open_in_memory().expect("db");
    let row = session_row("locale-probe", "local-tmux");
    let id = row.id;
    db.upsert_session(&row).expect("persist");

    let plain = capture_when_ready(&db, id, false);
    tmux(&["kill-server"]);

    assert_eq!(
        plain["cursor_row"].as_u64(),
        Some(1),
        "the cursor position must survive a C locale: {plain}"
    );
    assert_eq!(
        plain["foreground_cwd"].as_str().map(std::path::Path::new),
        Some(workdir_path.as_path()),
        "the live cwd must survive a C locale: {plain}"
    );
    assert!(
        plain["foreground_process"]
            .as_str()
            .is_some_and(|p| !p.is_empty()),
        "the foreground process must survive a C locale: {plain}"
    );
}
