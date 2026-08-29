//! `session send` and `session key` against a real tmux pane.
//!
//! The two things worth proving cannot be proved without a terminal: that text
//! sent with `--no-enter` is *typed but not submitted*, and that a named key
//! arrives as that key rather than as its own name typed in as text (which is
//! what tmux does with a name it does not recognize).
//!
//! Skipped when tmux is absent — a missing multiplexer is an environment fact,
//! not a regression — and scoped to a throwaway socket in a private directory,
//! so it can never touch a real session.

use std::process::Command;

use thurbox::cli::sessions::{run, Action};
use thurbox::session::SessionId;
use thurbox::storage::Database;
use thurbox::sync::SharedSession;

/// A throwaway tmux socket, so this never touches the real one.
const SOCKET: &str = "thurbox-send-keys-e2e";

/// The pane runs `cat`: with no shell in the way, the tty echoes what is typed
/// and `cat` writes the line back only once it is *submitted*. So "appears
/// once" and "appears twice" is the difference between typed and sent, read off
/// the screen rather than out of the implementation.
const PANE_PROGRAM: &str = "cat";

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", SOCKET])
        .args(args)
        .output()
        .expect("run tmux")
}

/// Point the CLI's one-shot helpers at a private socket in a private directory
/// so they can never see — or race — the shared dev server. nextest runs one
/// process per test, so the env mutation is safe. Returns the tempdir so it
/// outlives the test.
fn isolate_tmux() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TMUX_TMPDIR", dir.path());
    std::env::set_var(thurbox::agent::tmux::SOCKET_OVERRIDE_ENV, SOCKET);
    dir
}

fn cleanup() {
    let _ = tmux(&["kill-server"]);
}

/// A session row pointing at a live `tb-probe` pane, or `None` when tmux would
/// not start one.
///
/// Deliberately not `spawn_session_headless`: what is under test is the two
/// input commands, and a window plus a row is the whole of the state they read.
fn live_session(db: &Database) -> Option<SharedSession> {
    let out = tmux(&[
        "new-session",
        "-d",
        "-s",
        "probe",
        "-n",
        "tb-probe",
        "-x",
        "200",
        "-y",
        "50",
        "-P",
        "-F",
        "#{pane_id}",
        PANE_PROGRAM,
    ]);
    if !out.status.success() {
        return None;
    }
    let pane_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let session = SharedSession {
        id: SessionId::default(),
        name: "probe".into(),
        agent: "cat".into(),
        backend_id: pane_id,
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
    db.upsert_session(&session).expect("persist the row");
    Some(session)
}

/// What the pane shows right now.
fn screen(session: &SharedSession) -> String {
    let out = tmux(&["capture-pane", "-p", "-t", &session.backend_id]);
    assert!(out.status.success(), "capture-pane failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The pane once `settled` holds of it, or the last screen seen after a few
/// seconds of waiting — `send-keys` returns as soon as tmux has queued the
/// bytes, not once the program has echoed them, so every assertion about the
/// screen has to allow for that gap rather than sleep a guessed amount.
fn screen_when(session: &SharedSession, settled: impl Fn(&str) -> bool) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let screen = screen(session);
        if settled(&screen) || std::time::Instant::now() >= deadline {
            return screen;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn no_enter_types_without_submitting_and_key_enter_submits() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _tmux_dir = isolate_tmux();
    let db = Database::open_in_memory().expect("db");
    let Some(session) = live_session(&db) else {
        eprintln!("skipping: tmux would not spawn a window");
        return;
    };

    let out = run(
        Action::Send {
            uuid: session.id.to_string(),
            text: "READY_TOKEN".into(),
            no_enter: true,
        },
        &db,
    )
    .expect("send --no-enter");
    assert_eq!(out["sent"], true);
    assert_eq!(out["submitted"], false);

    // Typed: the tty echoed it. Not submitted: `cat` has not written it back.
    let typed = screen_when(&session, |s| occurrences(s, "READY_TOKEN") >= 1);
    assert_eq!(
        occurrences(&typed, "READY_TOKEN"),
        1,
        "the text should be on the pane exactly once — typed, not sent; shows:\n{typed}"
    );

    let out = run(
        Action::Key {
            uuid: session.id.to_string(),
            key: "enter".into(),
        },
        &db,
    )
    .expect("key enter");
    assert_eq!(out["sent"], true);
    assert_eq!(out["key"], "enter");

    // Submitted: `cat` echoed the line, so it is on the screen twice.
    let sent = screen_when(&session, |s| occurrences(s, "READY_TOKEN") >= 2);
    assert_eq!(
        occurrences(&sent, "READY_TOKEN"),
        2,
        "`key enter` should have submitted the line; shows:\n{sent}"
    );

    cleanup();
}

#[test]
fn text_arrives_literally_whatever_it_starts_with() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _tmux_dir = isolate_tmux();
    let db = Database::open_in_memory().expect("db");
    let Some(session) = live_session(&db) else {
        eprintln!("skipping: tmux would not spawn a window");
        return;
    };

    // A leading `-` is the trap: unwrapped, tmux reads it as a `send-keys`
    // flag. The rest is everything an integrator's steer tends to carry —
    // quotes, a `$`, a `;` and a `#` — none of which any shell should see.
    let text = r#"-n --literal "quo'ted" $HOME; # done"#;
    let out = run(
        Action::Send {
            uuid: session.id.to_string(),
            text: text.into(),
            no_enter: true,
        },
        &db,
    )
    .expect("send --no-enter");
    assert_eq!(out["submitted"], false);

    let screen = screen_when(&session, |s| s.contains(text));
    assert!(
        screen.contains(text),
        "the text should arrive intact; pane shows:\n{screen}"
    );

    cleanup();
}

#[test]
fn a_named_key_arrives_as_a_key_not_as_its_name() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _tmux_dir = isolate_tmux();
    let db = Database::open_in_memory().expect("db");
    let Some(session) = live_session(&db) else {
        eprintln!("skipping: tmux would not spawn a window");
        return;
    };

    run(
        Action::Send {
            uuid: session.id.to_string(),
            text: "DISCARD_ME".into(),
            no_enter: true,
        },
        &db,
    )
    .expect("send --no-enter");
    screen_when(&session, |s| s.contains("DISCARD_ME"));

    // `ctrl-u` is the tty's kill-line: the typed line goes away. If the key
    // name had been passed through unresolved, tmux would have typed the
    // *name* into the pane instead — which is the failure this pins.
    let out = run(
        Action::Key {
            uuid: session.id.to_string(),
            key: "CTRL+U".into(),
        },
        &db,
    )
    .expect("key ctrl-u");
    assert_eq!(
        out["key"], "ctrl-u",
        "the canonical spelling is echoed back"
    );
    assert_eq!(out["tmux_key"], "C-u");

    let screen = screen_when(&session, |s| !s.contains("DISCARD_ME"));
    for typed in ["CTRL+U", "ctrl-u", "C-u"] {
        assert!(
            !screen.contains(typed),
            "the key name must not land in the pane as text; shows:\n{screen}"
        );
    }
    assert!(
        !screen.contains("DISCARD_ME"),
        "ctrl-u should have killed the typed line; shows:\n{screen}"
    );

    cleanup();
}

#[test]
fn an_unknown_key_is_refused_before_anything_reaches_the_pane() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let _tmux_dir = isolate_tmux();
    let db = Database::open_in_memory().expect("db");
    let Some(session) = live_session(&db) else {
        eprintln!("skipping: tmux would not spawn a window");
        return;
    };

    let err = run(
        Action::Key {
            uuid: session.id.to_string(),
            key: "Escpe".into(),
        },
        &db,
    )
    .unwrap_err();
    assert!(err.contains("Unknown key"), "got {err}");
    let screen = screen(&session);
    assert!(
        !screen.contains("Escpe"),
        "a refused key must not have been typed into the pane; shows:\n{screen}"
    );

    cleanup();
}
